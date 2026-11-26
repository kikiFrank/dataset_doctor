//! Supervise the authenticated Worker and dispatch typed, durable attempts.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dd_core::execution_grant::{GrantBook, GrantError, GrantScope};
use dd_core::workspace::WorkspaceSession;
use dd_protocol::v1::{
    ArtifactRef, ColorPreviewInput, ColorPreviewRequest, DatasetProfileInput,
    DatasetProfileRequest, ExecutionHandle, ExecutionLookup, FrameResolveInput,
    FrameResolveRequest, GeometryAuditInput, GeometryAuditRequest, TaskContext, VideoProbeInput,
    VideoProbeRequest, VideoTrackingInput, VideoTrackingRequest, artifact_ref::Owner,
    core_execution_control_server::CoreExecutionControlServer,
};
use prost::Message;
use rusqlite::params;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_stream::wrappers::TcpListenerStream;
use uuid::Uuid;

use crate::execution_rpc::CoreClaimService;
use crate::input_fingerprint::InputFingerprintError;
use crate::paths::scoped_directory;
use crate::task_store::{TaskOutcome, TaskStoreError, format_digest};
use crate::worker_process::{CoreClaimEndpoint, WorkerLaunch, WorkerProcess, WorkerProcessError};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const GRANT_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum WorkerRuntimeError {
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
    TaskEvidence(Box<dyn Error + Send + Sync>),
}

impl Display for WorkerRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(error) => Display::fmt(error, formatter),
            Self::Worker(error) => Display::fmt(error, formatter),
            Self::Fingerprint(error) => Display::fmt(error, formatter),
            Self::Grant(error) => Display::fmt(error, formatter),
            Self::Io(error) => write!(formatter, "Prepare Workspace task files: {error}"),
            Self::CoreStopped => formatter.write_str("Core Worker service did not complete"),
            Self::StaleWorkspace => {
                formatter.write_str("Workspace session changed before execution")
            }
            Self::InvalidBinding => {
                formatter.write_str("Workspace task has an invalid file binding or option")
            }
            Self::CapabilityUnavailable => {
                formatter.write_str("Worker did not load the requested task capability")
            }
            Self::Deadline => formatter.write_str("Worker task timed out"),
            Self::ResultUnconfirmed => {
                formatter.write_str("Core did not accept an authoritative task result")
            }
            Self::TaskEvidence(error) => {
                write!(formatter, "Read authoritative task evidence: {error}")
            }
        }
    }
}

impl Error for WorkerRuntimeError {}

impl From<io::Error> for WorkerRuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<WorkspaceThreadError> for WorkerRuntimeError {
    fn from(error: WorkspaceThreadError) -> Self {
        Self::Workspace(error)
    }
}

impl From<WorkerProcessError> for WorkerRuntimeError {
    fn from(error: WorkerProcessError) -> Self {
        Self::Worker(error)
    }
}

pub(crate) struct WorkerRuntime {
    pub(crate) workspace: Arc<WorkspaceThread>,
    pub(crate) session: WorkspaceSession,
    root: PathBuf,
    executor: Uuid,
    grants: Arc<Mutex<GrantBook>>,
    worker: WorkerProcess,
    stop_core: oneshot::Sender<()>,
    core_task: JoinHandle<Result<(), tonic::transport::Error>>,
}

pub(crate) struct WorkerJob {
    pub(crate) task: Uuid,
    pub(crate) input_fingerprint: [u8; 32],
    pub(crate) manifest_sha: [u8; 32],
    pub(crate) manifest_size: u64,
    pub(crate) operation: WorkerOperation,
}

pub(crate) enum WorkerOperation {
    Geometry(GeometryAuditInput),
    DatasetProfile(DatasetProfileInput),
    ColorPreview(ColorPreviewInput),
    VideoProbe(VideoProbeInput),
    FrameResolve(FrameResolveInput),
    VideoTracking(VideoTrackingInput),
}

impl WorkerOperation {
    fn capability(&self) -> &'static str {
        match self {
            Self::Geometry(_) => crate::geometry_runtime::CAPABILITY,
            Self::DatasetProfile(_) => crate::dataset_profile_runtime::CAPABILITY,
            Self::ColorPreview(_) => crate::color_preview::CAPABILITY,
            Self::VideoProbe(_) => crate::video_registration::CAPABILITY,
            Self::FrameResolve(_) => crate::frame_runtime::CAPABILITY,
            Self::VideoTracking(_) => crate::video_tracking_runtime::CAPABILITY,
        }
    }

    fn bind(&mut self, output: String) -> [u8; 32] {
        let domain: &[u8];
        let encoded = match self {
            Self::Geometry(input) => {
                input.output_directory = output;
                domain = b"datasetdoctor.v1.PythonWorker.StartAnnotationGeometryAudit\0";
                input.encode_to_vec()
            }
            Self::DatasetProfile(input) => {
                input.output_directory = output;
                domain = b"datasetdoctor.v1.PythonWorker.StartProfileDataset\0";
                input.encode_to_vec()
            }
            Self::ColorPreview(input) => {
                input.output_directory = output;
                domain = b"datasetdoctor.v1.PythonWorker.StartColorPreview\0";
                input.encode_to_vec()
            }
            Self::FrameResolve(input) => {
                input.output_directory = output;
                domain = b"datasetdoctor.v1.PythonWorker.StartFrameResolve\0";
                input.encode_to_vec()
            }
            Self::VideoProbe(input) => {
                input.output_directory = output;
                domain = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0";
                input.encode_to_vec()
            }
            Self::VideoTracking(input) => {
                input.output_directory = output;
                domain = b"datasetdoctor.v1.PythonWorker.StartTracking\0";
                input.encode_to_vec()
            }
        };
        let mut operation = Sha256::new();
        operation.update(domain);
        operation.update(encoded);
        operation.finalize().into()
    }

    async fn start(
        self,
        worker: &mut WorkerProcess,
        context: TaskContext,
        manifest: ArtifactRef,
    ) -> Result<ExecutionHandle, WorkerProcessError> {
        match self {
            Self::Geometry(parameters) => {
                worker
                    .start_geometry(GeometryAuditRequest {
                        context: Some(context),
                        input_manifest_ref: Some(manifest),
                        parameters: Some(parameters),
                    })
                    .await
            }
            Self::DatasetProfile(parameters) => {
                worker
                    .start_dataset_profile(DatasetProfileRequest {
                        context: Some(context),
                        input_manifest_ref: Some(manifest),
                        parameters: Some(parameters),
                    })
                    .await
            }
            Self::ColorPreview(parameters) => {
                worker
                    .start_color_preview(ColorPreviewRequest {
                        context: Some(context),
                        input_manifest_ref: Some(manifest),
                        parameters: Some(parameters),
                    })
                    .await
            }
            Self::FrameResolve(parameters) => {
                worker
                    .start_frame_resolve(FrameResolveRequest {
                        context: Some(context),
                        input_manifest_ref: Some(manifest),
                        parameters: Some(parameters),
                    })
                    .await
            }
            Self::VideoProbe(parameters) => {
                worker
                    .start_video_probe(VideoProbeRequest {
                        context: Some(context),
                        input_manifest_ref: Some(manifest),
                        parameters: Some(parameters),
                    })
                    .await
            }
            Self::VideoTracking(parameters) => {
                worker
                    .start_tracking(VideoTrackingRequest {
                        context: Some(context),
                        input_manifest_ref: Some(manifest),
                        parameters: Some(parameters),
                    })
                    .await
            }
        }
    }
}

impl WorkerRuntime {
    pub(crate) fn task_outcome(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        capability: &'static str,
    ) -> Result<TaskOutcome, WorkspaceThreadError> {
        workspace.workspace_action(session, move |owner| {
            let exists: bool = owner
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=?1 AND capability_id=?2)",
                    params![task.to_string(), capability],
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
    pub(crate) fn record_unavailable(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        capability: &'static str,
        startup_failed: bool,
    ) -> Result<(), WorkerRuntimeError> {
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
                params![task.to_string(), session.to_string(), capability, status, code, now],
            ).map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Database(error)))?;
            Ok(())
        })?;
        Ok(())
    }

    pub(crate) async fn launch(
        workspace: Arc<WorkspaceThread>,
        expected_session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
        capability: &str,
    ) -> Result<Self, WorkerRuntimeError> {
        let storage = Arc::clone(&workspace);
        let (session, root) = tokio::task::spawn_blocking(move || {
            let session = storage
                .current()?
                .filter(|current| current.workspace_session_id == expected_session)
                .ok_or(WorkerRuntimeError::StaleWorkspace)?;
            let root = storage.storage_root(expected_session)?;
            Ok::<_, WorkerRuntimeError>((session, root))
        })
        .await
        .map_err(|_| WorkerRuntimeError::CoreStopped)??;
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
            return Err(WorkerRuntimeError::CoreStopped);
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
            .any(|id| id == capability)
        {
            return Err(WorkerRuntimeError::CapabilityUnavailable);
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

    pub(crate) async fn execute(
        &mut self,
        mut job: WorkerJob,
        deadline: Duration,
    ) -> Result<TaskOutcome, WorkerRuntimeError> {
        let session = self.session.workspace_session_id;
        let task = job.task;
        let latest = self.outcome(task).await?;
        if latest.status != "QUEUED" {
            return Ok(latest);
        }
        let attempt = Uuid::new_v4();
        let root = self.root.clone();
        let destination = PathBuf::from(format!("artifacts/tasks/{task}/{attempt}"));
        let output = tokio::task::spawn_blocking(move || scoped_directory(&root, &destination))
            .await
            .map_err(|_| WorkerRuntimeError::CoreStopped)??;
        let operation_digest = job.operation.bind(
            output
                .to_str()
                .ok_or(WorkerRuntimeError::InvalidBinding)?
                .to_owned(),
        );
        let scope = GrantScope {
            workspace_session: session,
            task,
            attempt,
            executor_session: self.executor,
            input_fingerprint: job.input_fingerprint,
            operation_digest,
            reservation: Uuid::new_v4(),
        };
        let node_spec: [u8; 32] = Sha256::digest(job.operation.capability().as_bytes()).into();
        let grant = self
            .grants
            .lock()
            .map_err(|_| WorkerRuntimeError::CoreStopped)?
            .install(scope.clone(), Instant::now() + GRANT_LIFETIME)
            .map_err(WorkerRuntimeError::Grant)?;
        let workspace = Arc::clone(&self.workspace);
        let dispatched = tokio::task::spawn_blocking(move || {
            Ok::<_, WorkerRuntimeError>(workspace.record_dispatch(
                scope,
                grant,
                node_spec,
                now_ms()?,
            )?)
        })
        .await
        .map_err(|_| WorkerRuntimeError::CoreStopped);
        if let Err(error) = dispatched.and_then(|result| result) {
            self.grants
                .lock()
                .map_err(|_| WorkerRuntimeError::CoreStopped)?
                .revoke_unclaimed(grant);
            return Err(error);
        }
        let context = TaskContext {
            workspace_id: self.session.workspace_id.clone(),
            workspace_session_id: session.to_string(),
            task_id: task.to_string(),
            attempt_id: attempt.to_string(),
            executor_session_id: self.executor.to_string(),
            input_fingerprint: format_digest(&job.input_fingerprint),
            node_spec_fingerprint: format_digest(&node_spec),
            grant_id: grant.to_string(),
            ..Default::default()
        };
        let manifest = ArtifactRef {
            artifact_id: Uuid::new_v4().to_string(),
            owner: Some(Owner::OwnerTaskId(task.to_string())),
            kind: "INPUT_MANIFEST".into(),
            media_type: "application/json".into(),
            size_bytes: job.manifest_size,
            sha256: format_digest(&job.manifest_sha),
            schema_id: Some(
                "https://datasetdoctor.invalid/contracts/json-schema/v1/input-manifest.schema.json"
                    .into(),
            ),
        };
        let handle = job
            .operation
            .start(&mut self.worker, context, manifest)
            .await?;
        timeout(deadline, async {
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
        .map_err(|_| WorkerRuntimeError::Deadline)??;
        let result = self.outcome(task).await?;
        if !matches!(result.status.as_str(), "SUCCEEDED" | "FAILED") {
            return Err(WorkerRuntimeError::ResultUnconfirmed);
        }
        Ok(result)
    }

    pub(crate) async fn outcome(&self, task: Uuid) -> Result<TaskOutcome, WorkerRuntimeError> {
        let workspace = Arc::clone(&self.workspace);
        let session = self.session.workspace_session_id;
        Ok(
            tokio::task::spawn_blocking(move || workspace.task_outcome(session, task))
                .await
                .map_err(|_| WorkerRuntimeError::CoreStopped)??,
        )
    }

    pub(crate) async fn shutdown(self) -> Result<(), WorkerRuntimeError> {
        let worker = self.worker.shutdown().await;
        let core = Self::stop_service(self.stop_core, self.core_task).await;
        worker?;
        core
    }

    pub(crate) async fn terminate(self) -> Result<(), WorkerRuntimeError> {
        let worker = self.worker.terminate().await;
        let core = Self::stop_service(self.stop_core, self.core_task).await;
        worker?;
        core
    }

    async fn stop_service(
        stop: oneshot::Sender<()>,
        task: JoinHandle<Result<(), tonic::transport::Error>>,
    ) -> Result<(), WorkerRuntimeError> {
        let _ = stop.send(());
        timeout(GRANT_LIFETIME, task)
            .await
            .map_err(|_| WorkerRuntimeError::Deadline)?
            .map_err(|_| WorkerRuntimeError::CoreStopped)?
            .map_err(|_| WorkerRuntimeError::CoreStopped)
    }
}

pub(crate) fn now_ms() -> Result<i64, WorkerRuntimeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .ok_or(WorkerRuntimeError::CoreStopped)
}
