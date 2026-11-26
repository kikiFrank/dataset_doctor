//! Private gRPC adapter for a supervised executor's Core grant requests.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dd_core::execution_grant::{ClaimContext, GrantBook, GrantError};
use dd_core::workspace::WorkspaceAccessError;
use dd_protocol::v1::{
    ClaimGrantRequest, ExecutionHandle, ReportAck, ReportExecutionRequest, TerminalResultReceipt,
    core_execution_control_server::CoreExecutionControl, report_execution_request::Event,
};
use subtle::ConstantTimeEq;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::artifact_manifest::{TaskManifestScope, verify_task_result_manifest};
use crate::json::JsonContracts;
use crate::task_store::{ReportScope, ResultSubmission, TaskStoreError};
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

    fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
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
            workspace
                .record_claimed(claim.workspace_session, grant, receipt, utc_now_ms()?)
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

    async fn report_execution(
        &self,
        request: Request<ReportExecutionRequest>,
    ) -> Result<Response<ReportAck>, Status> {
        self.authenticate(&request)?;
        let request = request.into_inner();
        let context = request
            .context
            .ok_or_else(|| Status::invalid_argument("Execution context is required"))?;
        if parse_uuid(&context.executor_session_id)? != self.executor {
            return Err(Status::permission_denied(
                "Executor session does not own this connection",
            ));
        }
        if request.source_event_seq == 0 {
            return Err(Status::invalid_argument(
                "Execution event sequence starts at one",
            ));
        }
        let claim = ClaimContext {
            workspace_session: parse_uuid(&context.workspace_session_id)?,
            task: parse_uuid(&context.task_id)?,
            attempt: parse_uuid(&context.attempt_id)?,
            execution: parse_uuid(&request.worker_execution_id)?,
            input_fingerprint: parse_digest(&context.input_fingerprint)?,
            operation_digest: parse_digest(&request.operation_digest)?,
        };
        let grant = parse_uuid(&context.grant_id)?;
        let workspace_id = parse_uuid(&context.workspace_id)?;
        let node_spec_fingerprint = parse_digest(&context.node_spec_fingerprint)?;
        let event_id = parse_uuid(&request.execution_event_id)?;
        let source_event_seq = request.source_event_seq;
        let event = request
            .event
            .ok_or_else(|| Status::invalid_argument("Execution event is required"))?;
        let executor = self.executor;
        let grants = Arc::clone(&self.grants);
        let workspace = Arc::clone(&self.workspace);

        let terminal_receipt = tokio::task::spawn_blocking(
            move || -> Result<Option<TerminalResultReceipt>, Status> {
                let execution = grants
                    .lock()
                    .map_err(|_| Status::internal("Core grant state unavailable"))?
                    .validate_claim(grant, executor, &claim, Instant::now())
                    .map_err(claim_status)?;
                let directory = workspace
                    .report_directory(ReportScope {
                        workspace: workspace_id,
                        workspace_session: claim.workspace_session,
                        grant,
                        execution,
                        input_fingerprint: claim.input_fingerprint,
                        node_spec_fingerprint,
                    })
                    .map_err(storage_status)?;
                match event {
                    Event::Started(_) => {
                        workspace
                            .record_reported_running(
                                claim.workspace_session,
                                grant,
                                execution,
                                (event_id, source_event_seq),
                                utc_now_ms()?,
                            )
                            .map_err(storage_status)?;
                        Ok(None)
                    }
                    Event::Succeeded(success) => {
                        let reference = success.result_manifest_ref.ok_or_else(|| {
                            Status::invalid_argument(
                                "Verified result manifest reference is required",
                            )
                        })?;
                        let contracts = JsonContracts::new()
                            .map_err(|_| Status::internal("Core result validator unavailable"))?;
                        let verified = verify_task_result_manifest(
                            &directory,
                            &contracts,
                            &TaskManifestScope {
                                task: claim.task,
                                attempt: claim.attempt,
                                input_fingerprint: claim.input_fingerprint,
                            },
                            &reference,
                        )
                        .map_err(|_| {
                            Status::failed_precondition("Core rejected the result manifest")
                        })?;
                        let receipt = workspace
                            .record_success(
                                ResultSubmission {
                                    workspace_session: claim.workspace_session,
                                    grant,
                                    execution,
                                    node_spec_fingerprint,
                                    execution_event_id: event_id,
                                    source_event_seq,
                                    verified,
                                },
                                utc_now_ms()?,
                            )
                            .map_err(storage_status)?;
                        Ok(Some(TerminalResultReceipt {
                            accepted_at: receipt.accepted_at,
                            completion_fingerprint: receipt.completion_fingerprint,
                            result_manifest_ref: Some(reference),
                        }))
                    }
                }
            },
        )
        .await
        .map_err(|_| Status::internal("Core report did not complete"))??;
        Ok(Response::new(ReportAck {
            accepted: true,
            execution_event_id: event_id.to_string(),
            terminal_receipt,
        }))
    }
}

fn utc_now_ms() -> Result<i64, Status> {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Status::internal("Core clock unavailable"))?;
    i64::try_from(epoch.as_millis()).map_err(|_| Status::internal("Core clock unavailable"))
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
