//! Private gRPC adapter for a supervised executor's Core grant requests.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dd_core::execution_grant::{ClaimContext, GrantBook, GrantError};
use dd_core::workspace::WorkspaceAccessError;
use dd_protocol::v1::{
    ClaimGrantRequest, ExecutionHandle, core_execution_control_server::CoreExecutionControl,
};
use subtle::ConstantTimeEq;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::task_store::TaskStoreError;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub struct CoreClaimService {
    executor: Uuid,
    authorization: String,
    grants: Arc<Mutex<GrantBook>>,
    workspace: Arc<WorkspaceThread>,
}

impl CoreClaimService {
    pub fn new(
        executor: Uuid,
        credential: Uuid,
        grants: Arc<Mutex<GrantBook>>,
        workspace: Arc<WorkspaceThread>,
    ) -> Self {
        Self {
            executor,
            authorization: format!("Bearer {}", credential.simple()),
            grants,
            workspace,
        }
    }

    fn authenticate(&self, request: &Request<ClaimGrantRequest>) -> Result<(), Status> {
        let all = request.metadata().get_all("authorization");
        let mut values = all.iter();
        let token = values.next().ok_or_else(invalid_credential)?;
        if values.next().is_some()
            || !bool::from(
                token
                    .as_encoded_bytes()
                    .ct_eq(self.authorization.as_bytes()),
            )
        {
            return Err(invalid_credential());
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl CoreExecutionControl for CoreClaimService {
    async fn claim_grant(
        &self,
        request: Request<ClaimGrantRequest>,
    ) -> Result<Response<ExecutionHandle>, Status> {
        self.authenticate(&request)?;
        let request = request.into_inner();
        if parse_uuid(&request.executor_session_id)? != self.executor {
            return Err(Status::permission_denied(
                "Executor session does not own this connection",
            ));
        }
        let claim = ClaimContext {
            workspace_session: parse_uuid(&request.workspace_session_id)?,
            task: parse_uuid(&request.task_id)?,
            attempt: parse_uuid(&request.attempt_id)?,
            execution: parse_uuid(&request.worker_execution_id)?,
            input_fingerprint: parse_digest(&request.input_fingerprint)?,
            operation_digest: parse_digest(&request.operation_digest)?,
        };
        let grant = parse_uuid(&request.grant_id)?;
        let grants = Arc::clone(&self.grants);
        let workspace = Arc::clone(&self.workspace);
        let executor = self.executor;
        let receipt = tokio::task::spawn_blocking(move || {
            let mut grants = grants
                .lock()
                .map_err(|_| Status::internal("Core grant state unavailable"))?;
            let now = Instant::now();
            let receipt = grants
                .validate_claim(grant, executor, &claim, now)
                .map_err(claim_status)?;
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Status::internal("Core clock unavailable"))?;
            let now_ms = i64::try_from(epoch.as_millis())
                .map_err(|_| Status::internal("Core clock unavailable"))?;
            workspace
                .record_claimed(claim.workspace_session, grant, receipt, now_ms)
                .map_err(storage_status)?;
            grants
                .claim(grant, executor, &claim, now)
                .map_err(claim_status)
        })
        .await
        .map_err(|_| Status::internal("Core claim did not complete"))??;
        Ok(Response::new(ExecutionHandle {
            task_id: receipt.task.to_string(),
            attempt_id: receipt.attempt.to_string(),
            worker_execution_id: receipt.execution.to_string(),
        }))
    }
}

fn parse_uuid(value: &str) -> Result<Uuid, Status> {
    let uuid = Uuid::parse_str(value)
        .map_err(|_| Status::invalid_argument("Invalid execution identifier"))?;
    if uuid.hyphenated().encode_lower(&mut Uuid::encode_buffer()) != value {
        return Err(Status::invalid_argument(
            "Expected canonical execution identifier",
        ));
    }
    Ok(uuid)
}

fn parse_digest(value: &str) -> Result<[u8; 32], Status> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Status::invalid_argument(
            "Expected lower-case SHA-256 digest",
        ));
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| Status::invalid_argument("Expected lower-case SHA-256 digest"))?;
    }
    Ok(digest)
}

fn invalid_credential() -> Status {
    Status::unauthenticated("Invalid Core execution credential")
}

fn claim_status(error: GrantError) -> Status {
    match error {
        GrantError::ScopeMismatch => Status::permission_denied("Execution scope does not match"),
        GrantError::AlreadyClaimed => Status::already_exists("Grant is already claimed"),
        other => Status::failed_precondition(other.to_string()),
    }
}

fn storage_status(error: WorkspaceThreadError) -> Status {
    match error {
        WorkspaceThreadError::Access(
            WorkspaceAccessError::StaleSession | WorkspaceAccessError::Closed,
        )
        | WorkspaceThreadError::Task(TaskStoreError::AttemptConflict) => {
            Status::failed_precondition("Task attempt is no longer current")
        }
        WorkspaceThreadError::Busy => Status::resource_exhausted("Core task queue is busy"),
        _ => Status::internal("Core could not persist this claim"),
    }
}
