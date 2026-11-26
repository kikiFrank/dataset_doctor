//! Run a real Worker geometry audit for an image registered in a Workspace.

mod review_image;

pub use review_image::{
    GeometryFinding, GeometryImage, GeometryReport, ReviewImageEvidenceError, VerifiedReviewImage,
    read_geometry_report, read_review_image,
};

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dd_core::execution_grant::{GrantBook, GrantError, GrantScope};
use dd_core::workspace::WorkspaceSession;
use dd_protocol::v1::{
    ArtifactRef, ExecutionLookup, GeometryAuditInput, GeometryAuditRequest as WorkerRequest,
    TaskContext, WorkerInputFile, artifact_ref::Owner,
    core_execution_control_server::CoreExecutionControlServer,
};
use prost::Message;
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_stream::wrappers::TcpListenerStream;
use uuid::Uuid;

use crate::dataset_query::{RegisteredImage, RegisteredInputFile};
use crate::execution_rpc::CoreClaimService;
use crate::input_fingerprint::{InputFingerprintError, freeze_input_manifest};
use crate::json::JsonContracts;
use crate::paths::{publish_immutable, scoped_directory};
use crate::task_store::{StoredTask, TaskOutcome, TaskStoreError, TaskSubmission, format_digest};
use crate::worker_process::{CoreClaimEndpoint, WorkerLaunch, WorkerProcess, WorkerProcessError};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const CAPABILITY: &str = "annotation_geometry.v1";
const GRANT_LIFETIME: Duration = Duration::from_secs(10);
const TASK_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct GeometryAuditRequest {
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub max_pixels: u64,
}

#[derive(Debug)]
pub enum GeometryRuntimeError {
    Workspace(WorkspaceThreadError),
    Worker(WorkerProcessError),
    Fingerprint(InputFingerprintError),
    Grant(GrantError),
    Io(io::Error),
    CoreStopped,
    StaleWorkspace,
    InvalidBinding,
    CapabilityUnavailable,
    Deadline,
    ResultUnconfirmed,
}

impl Display for GeometryRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(error) => Display::fmt(error, formatter),
            Self::Worker(error) => Display::fmt(error, formatter),
            Self::Fingerprint(error) => Display::fmt(error, formatter),
            Self::Grant(error) => Display::fmt(error, formatter),
            Self::Io(error) => write!(formatter, "Prepare Workspace audit files: {error}"),
            Self::CoreStopped => formatter.write_str("Core audit service did not complete"),
            Self::StaleWorkspace => {
                formatter.write_str("Workspace session changed before the audit")
            }
            Self::InvalidBinding => {
                formatter.write_str("Workspace audit has an invalid file binding or option")
            }
            Self::CapabilityUnavailable => {
                formatter.write_str("Worker did not load the geometry audit capability")
            }
            Self::Deadline => formatter.write_str("Geometry audit timed out"),
            Self::ResultUnconfirmed => {
                formatter.write_str("Core did not accept an authoritative audit result")
            }
        }
    }
}

impl Error for GeometryRuntimeError {}

impl From<io::Error> for GeometryRuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<WorkspaceThreadError> for GeometryRuntimeError {
    fn from(error: WorkspaceThreadError) -> Self {
        Self::Workspace(error)
    }
}

impl From<WorkerProcessError> for GeometryRuntimeError {
    fn from(error: WorkerProcessError) -> Self {
        Self::Worker(error)
    }
}

pub struct GeometryRuntime {
    workspace: Arc<WorkspaceThread>,
    session: WorkspaceSession,
    root: PathBuf,
    executor: Uuid,
    grants: Arc<Mutex<GrantBook>>,
    worker: WorkerProcess,
    stop_core: oneshot::Sender<()>,
    core_task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl GeometryRuntime {
    pub fn task_outcome(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
    ) -> Result<TaskOutcome, WorkspaceThreadError> {
        workspace.workspace_action(session, move |owner| {
            let exists: bool = owner
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND capability_id=?2)",
                    params![task.to_string(), CAPABILITY],
                    |row| row.get(0),
                )
                .map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Database(error)))?;
            if !exists {
                return Err(WorkspaceThreadError::Task(TaskStoreError::UnknownTask));
            }
            owner.task_outcome(task).map_err(WorkspaceThreadError::Task)
        })
    }

    /// A launch failure has no admitted executor. Once started, the caller
    /// must confirm that its Worker has stopped before marking an interruption.
    pub fn record_unavailable(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        startup_failed: bool,
    ) -> Result<(), GeometryRuntimeError> {
        let now = now_ms()?;
        let (status, code) = if startup_failed {
            ("FAILED", "WORKER_START_FAILED")
        } else {
            ("INTERRUPTED", "WORKER_INTERRUPTED")
        };
        workspace.workspace_action(session, move |owner| {
            owner.connection.execute(
                "UPDATE tasks SET status=?4, error_code=?5, finished_at_ms=?6, updated_at_ms=?6,
                    row_revision=row_revision+1,
                    execution_metadata_json=json_set(execution_metadata_json,'$.grant_state','REVOKED')
                 WHERE task_id=?1 AND workspace_session_id=?2 AND capability_id=?3
                   AND (status='QUEUED' OR (?4='INTERRUPTED' AND status IN ('DISPATCHING','RUNNING','CANCELLING')))",
                params![task.to_string(), session.to_string(), CAPABILITY, status, code, now],
            ).map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Database(error)))?;
            Ok(())
        })?;
        Ok(())
    }

    pub async fn launch(
        workspace: Arc<WorkspaceThread>,
        expected_session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, GeometryRuntimeError> {
        let storage = Arc::clone(&workspace);
        let (session, root) = tokio::task::spawn_blocking(move || {
            let session = storage
                .current()?
                .filter(|current| current.workspace_session_id == expected_session)
                .ok_or(GeometryRuntimeError::StaleWorkspace)?;
            let root = storage.storage_root(expected_session)?;
            Ok::<_, GeometryRuntimeError>((session, root))
        })
        .await
        .map_err(|_| GeometryRuntimeError::CoreStopped)??;
        let executor = Uuid::new_v4();
        let claim_token = Uuid::new_v4();
        let grants = Arc::new(Mutex::new(GrantBook::new(expected_session)));
        let core = CoreClaimService::new(
            executor,
            claim_token,
            Arc::clone(&grants),
            Arc::clone(&workspace),
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let SocketAddr::V4(address) = listener.local_addr()? else {
            return Err(GeometryRuntimeError::CoreStopped);
        };
        let (stop_core, stop) = oneshot::channel();
        let core_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(CoreExecutionControlServer::new(core))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = stop.await;
                })
                .await
        });
        let mut worker = WorkerProcess::launch(WorkerLaunch {
            interpreter,
            entrypoint,
            executor_session: executor,
            core: Some(CoreClaimEndpoint {
                address,
                claim_token,
            }),
        })
        .await?;
        if !worker
            .health()
            .await?
            .loaded_capability_ids
            .iter()
            .any(|id| id == CAPABILITY)
        {
            return Err(GeometryRuntimeError::CapabilityUnavailable);
        }
        Ok(Self {
            workspace,
            session,
            root,
            executor,
            grants,
            worker,
            stop_core,
            core_task,
        })
    }

    pub async fn audit(
        &mut self,
        request: GeometryAuditRequest,
    ) -> Result<TaskOutcome, GeometryRuntimeError> {
        let workspace = Arc::clone(&self.workspace);
        let session = self.session.workspace_session_id;
        let prepared =
            tokio::task::spawn_blocking(move || Self::admit(&workspace, session, request))
                .await
                .map_err(|_| GeometryRuntimeError::CoreStopped)??;
        self.audit_admitted(prepared).await
    }

    /// Freeze and enqueue while no Worker needs to be running.
    pub fn admit(
        workspace: &WorkspaceThread,
        session: Uuid,
        request: GeometryAuditRequest,
    ) -> Result<GeometryAuditJob, GeometryRuntimeError> {
        let root = workspace.storage_root(session)?;
        prepare(workspace, &root, session, request)
    }

    pub async fn audit_admitted(
        &mut self,
        prepared: GeometryAuditJob,
    ) -> Result<TaskOutcome, GeometryRuntimeError> {
        let session = self.session.workspace_session_id;
        if prepared.session != session {
            return Err(GeometryRuntimeError::StaleWorkspace);
        }
        let task = prepared.task.task_id;
        let latest = self.outcome(task).await?;
        if latest.status != "QUEUED" || prepared.task.workspace_session != session {
            return Ok(latest);
        }
        let attempt = Uuid::new_v4();
        let root = self.root.clone();
        let destination = PathBuf::from(format!("artifacts/tasks/{task}/{attempt}"));
        let output = tokio::task::spawn_blocking(move || scoped_directory(&root, &destination))
            .await
            .map_err(|_| GeometryRuntimeError::CoreStopped)??;
        let parameters = GeometryAuditInput {
            output_directory: path_text(&output)?,
            ..prepared.parameters
        };
        let mut operation = Sha256::new();
        // This transport digest binds exact Start parameters. The separately
        // frozen business fingerprint is the canonical JSON projection.
        operation.update(b"datasetdoctor.v1.PythonWorker.StartAnnotationGeometryAudit\0");
        operation.update(parameters.encode_to_vec());
        let scope = GrantScope {
            workspace_session: session,
            task,
            attempt,
            executor_session: self.executor,
            input_fingerprint: prepared.input_fingerprint,
            operation_digest: operation.finalize().into(),
            reservation: Uuid::new_v4(),
        };
        let node_spec: [u8; 32] = Sha256::digest(CAPABILITY.as_bytes()).into();
        let grant = self
            .grants
            .lock()
            .map_err(|_| GeometryRuntimeError::CoreStopped)?
            .install(scope.clone(), Instant::now() + GRANT_LIFETIME)
            .map_err(GeometryRuntimeError::Grant)?;
        let workspace = Arc::clone(&self.workspace);
        let dispatched = tokio::task::spawn_blocking(move || {
            Ok::<_, GeometryRuntimeError>(workspace.record_dispatch(
                scope,
                grant,
                node_spec,
                now_ms()?,
            )?)
        })
        .await
        .map_err(|_| GeometryRuntimeError::CoreStopped);
        if let Err(error) = dispatched.and_then(|result| result) {
            self.grants
                .lock()
                .map_err(|_| GeometryRuntimeError::CoreStopped)?
                .revoke_unclaimed(grant);
            return Err(error);
        }
        let handle = self.worker.start_geometry(WorkerRequest {
            context: Some(TaskContext {
                workspace_id: self.session.workspace_id.clone(), workspace_session_id: session.to_string(),
                task_id: task.to_string(), attempt_id: attempt.to_string(), executor_session_id: self.executor.to_string(),
                input_fingerprint: format_digest(&prepared.input_fingerprint), node_spec_fingerprint: format_digest(&node_spec),
                grant_id: grant.to_string(), ..Default::default()
            }),
            input_manifest_ref: Some(ArtifactRef {
                artifact_id: Uuid::new_v4().to_string(), owner: Some(Owner::OwnerTaskId(task.to_string())),
                kind: "INPUT_MANIFEST".into(), media_type: "application/json".into(),
                size_bytes: prepared.manifest_size, sha256: format_digest(&prepared.manifest_sha),
                schema_id: Some("https://datasetdoctor.invalid/contracts/json-schema/v1/input-manifest.schema.json".into()),
            }),
            parameters: Some(parameters),
        }).await?;
        timeout(TASK_DEADLINE, async {
            loop {
                let snapshot = self
                    .worker
                    .execution(ExecutionLookup {
                        task_id: handle.task_id.clone(),
                        attempt_id: handle.attempt_id.clone(),
                    })
                    .await?;
                if matches!(snapshot.status.as_str(), "SUCCEEDED" | "FAILED") {
                    break Ok::<_, WorkerProcessError>(());
                }
                sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .map_err(|_| GeometryRuntimeError::Deadline)??;
        let result = self.outcome(task).await?;
        if !matches!(result.status.as_str(), "SUCCEEDED" | "FAILED") {
            return Err(GeometryRuntimeError::ResultUnconfirmed);
        }
        Ok(result)
    }

    async fn outcome(&self, task: Uuid) -> Result<TaskOutcome, GeometryRuntimeError> {
        let workspace = Arc::clone(&self.workspace);
        let session = self.session.workspace_session_id;
        Ok(
            tokio::task::spawn_blocking(move || workspace.task_outcome(session, task))
                .await
                .map_err(|_| GeometryRuntimeError::CoreStopped)??,
        )
    }

    pub async fn shutdown(self) -> Result<(), GeometryRuntimeError> {
        let worker = self.worker.shutdown().await;
        let _ = self.stop_core.send(());
        let core = timeout(GRANT_LIFETIME, self.core_task).await;
        worker?;
        core.map_err(|_| GeometryRuntimeError::Deadline)?
            .map_err(|_| GeometryRuntimeError::CoreStopped)?
            .map_err(|_| GeometryRuntimeError::CoreStopped)
    }
}

pub struct GeometryAuditJob {
    session: Uuid,
    task: StoredTask,
    input_fingerprint: [u8; 32],
    manifest_sha: [u8; 32],
    manifest_size: u64,
    parameters: GeometryAuditInput,
}

impl GeometryAuditJob {
    pub fn task(&self) -> &StoredTask {
        &self.task
    }
}

fn prepare(
    workspace: &WorkspaceThread,
    root: &Path,
    session: Uuid,
    request: GeometryAuditRequest,
) -> Result<GeometryAuditJob, GeometryRuntimeError> {
    if request.max_pixels == 0 {
        return Err(GeometryRuntimeError::InvalidBinding);
    }
    let RegisteredImage {
        asset_id,
        dataset_id,
        image,
        annotation,
        label_schema_fingerprint,
    } = workspace.registered_image(session, request.dataset_id, request.asset_id)?;
    let contracts = JsonContracts::new().map_err(|_| GeometryRuntimeError::CoreStopped)?;
    let frozen = freeze_input_manifest(&contracts, json!({
        "dataset_id": dataset_id.to_string(), "label_schema_digest": label_schema_fingerprint,
        "assets": [{"asset_id": asset_id.to_string(), "media_digest": image.sha256,
            "annotation_digest": annotation.as_ref().map(|file| &file.sha256), "source_metadata_digest": null}],
        "capability_id": CAPABILITY, "implementation_version": env!("CARGO_PKG_VERSION"),
        "model_identity": null, "resolved_config": {"max_pixels": request.max_pixels},
    })).map_err(GeometryRuntimeError::Fingerprint)?;
    let directory = Path::new("artifacts/task-inputs");
    let name = format!("{}.json", format_digest(&frozen.sha256));
    let relative = directory.join(&name);
    publish_immutable(root, &relative, &frozen.bytes)?;
    let parameters = GeometryAuditInput {
        image: Some(locator(image)?),
        annotation: annotation.map(locator).transpose()?,
        input_manifest: Some(WorkerInputFile {
            path: path_text(&root.join(&relative))?,
            sha256: format_digest(&frozen.sha256),
        }),
        max_pixels: request.max_pixels,
        ..Default::default()
    };
    let mut request_digest = Sha256::new();
    request_digest.update(b"datasetdoctor.task.annotation-geometry.v1\0");
    request_digest.update(frozen.input_fingerprint);
    let task = workspace.enqueue_task(
        TaskSubmission {
            workspace_session: session,
            dataset_id: Some(dataset_id),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: "ANNOTATION_GEOMETRY".into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: relative,
            request_fingerprint: request_digest.finalize().into(),
            input_fingerprint: frozen.input_fingerprint,
        },
        now_ms()?,
    )?;
    Ok(GeometryAuditJob {
        session,
        task,
        input_fingerprint: frozen.input_fingerprint,
        manifest_sha: frozen.sha256,
        manifest_size: frozen.bytes.len() as u64,
        parameters,
    })
}

fn locator(file: RegisteredInputFile) -> Result<WorkerInputFile, GeometryRuntimeError> {
    Ok(WorkerInputFile {
        path: path_text(&file.path)?,
        sha256: file.sha256,
    })
}

fn path_text(path: &Path) -> Result<String, GeometryRuntimeError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(GeometryRuntimeError::InvalidBinding)
}

fn now_ms() -> Result<i64, GeometryRuntimeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .ok_or(GeometryRuntimeError::CoreStopped)
}
