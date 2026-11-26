//! Run a real Worker geometry audit for an image registered in a Workspace.

mod review_image;
pub(crate) use review_image::{BoundGeometryReport, read_bound_geometry_report};

pub use crate::worker_runtime::WorkerRuntimeError as GeometryRuntimeError;
pub use review_image::{
    GeometryFinding, GeometryImage, GeometryReport, ReviewImageEvidenceError, VerifiedReviewImage,
    read_geometry_report, read_review_image,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_protocol::v1::{GeometryAuditInput, WorkerInputFile};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dataset_query::{RegisteredImage, RegisteredInputFile};
use crate::input_fingerprint::{FrozenInputManifest, freeze_input_manifest};
use crate::json::JsonContracts;
use crate::paths::publish_immutable;
use crate::task_store::{StoredTask, TaskOutcome, TaskSubmission, format_digest};
use crate::worker_runtime::{WorkerJob, WorkerOperation, WorkerRuntime, now_ms};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub(crate) const CAPABILITY: &str = "annotation_geometry.v1";
const TASK_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct GeometryAuditRequest {
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub max_pixels: u64,
}

pub struct GeometryRuntime {
    runtime: WorkerRuntime,
}

impl GeometryRuntime {
    pub fn task_outcome(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
    ) -> Result<TaskOutcome, WorkspaceThreadError> {
        WorkerRuntime::task_outcome(workspace, session, task, CAPABILITY)
    }

    pub fn record_unavailable(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        startup_failed: bool,
    ) -> Result<(), GeometryRuntimeError> {
        WorkerRuntime::record_unavailable(workspace, session, task, CAPABILITY, startup_failed)
    }

    pub async fn launch(
        workspace: Arc<WorkspaceThread>,
        expected_session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, GeometryRuntimeError> {
        Ok(Self {
            runtime: WorkerRuntime::launch(
                workspace,
                expected_session,
                interpreter,
                entrypoint,
                CAPABILITY,
            )
            .await?,
        })
    }

    pub async fn audit(
        &mut self,
        request: GeometryAuditRequest,
    ) -> Result<TaskOutcome, GeometryRuntimeError> {
        let workspace = Arc::clone(&self.runtime.workspace);
        let session = self.runtime.session.workspace_session_id;
        let prepared =
            tokio::task::spawn_blocking(move || Self::admit(&workspace, session, request))
                .await
                .map_err(|_| GeometryRuntimeError::CoreStopped)??;
        self.audit_admitted(prepared).await
    }

    pub fn admit(
        workspace: &WorkspaceThread,
        session: Uuid,
        request: GeometryAuditRequest,
    ) -> Result<GeometryAuditJob, GeometryRuntimeError> {
        let prepared = Self::prepare(workspace, session, request)?;
        let task = workspace.enqueue_task(prepared.submission.clone(), now_ms()?)?;
        Ok(prepared.into_job(task))
    }

    pub(crate) fn prepare(
        workspace: &WorkspaceThread,
        session: Uuid,
        request: GeometryAuditRequest,
    ) -> Result<PreparedGeometry, GeometryRuntimeError> {
        let root = workspace.storage_root(session)?;
        prepare(workspace, &root, session, request)
    }

    pub async fn audit_admitted(
        &mut self,
        prepared: GeometryAuditJob,
    ) -> Result<TaskOutcome, GeometryRuntimeError> {
        let session = self.runtime.session.workspace_session_id;
        if prepared.session != session {
            return Err(GeometryRuntimeError::StaleWorkspace);
        }
        if prepared.task.workspace_session != session {
            return Ok(self.runtime.outcome(prepared.task.task_id).await?);
        }
        self.runtime
            .execute(
                WorkerJob {
                    task: prepared.task.task_id,
                    input_fingerprint: prepared.input_fingerprint,
                    manifest_sha: prepared.manifest_sha,
                    manifest_size: prepared.manifest_size,
                    operation: WorkerOperation::Geometry(prepared.parameters),
                },
                TASK_DEADLINE,
            )
            .await
    }

    pub async fn shutdown(self) -> Result<(), GeometryRuntimeError> {
        self.runtime.shutdown().await
    }

    pub async fn terminate(self) -> Result<(), GeometryRuntimeError> {
        self.runtime.terminate().await
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

pub(crate) struct PreparedGeometry {
    pub(crate) submission: TaskSubmission,
    frozen: FrozenInputManifest,
    parameters: GeometryAuditInput,
}

impl PreparedGeometry {
    pub(crate) fn manifest_bytes(&self) -> &[u8] {
        &self.frozen.bytes
    }

    pub(crate) fn into_job(self, task: StoredTask) -> GeometryAuditJob {
        GeometryAuditJob {
            session: self.submission.workspace_session,
            task,
            input_fingerprint: self.frozen.input_fingerprint,
            manifest_sha: self.frozen.sha256,
            manifest_size: self.frozen.bytes.len() as u64,
            parameters: self.parameters,
        }
    }
}

fn prepare(
    workspace: &WorkspaceThread,
    root: &Path,
    session: Uuid,
    request: GeometryAuditRequest,
) -> Result<PreparedGeometry, GeometryRuntimeError> {
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
    let submission = TaskSubmission {
        workspace_session: session,
        dataset_id: Some(dataset_id),
        request_id: request.request_id,
        idempotency_key: request.idempotency_key,
        operation_kind: "ANNOTATION_GEOMETRY".into(),
        capability_id: CAPABILITY.into(),
        input_manifest_path: relative,
        request_fingerprint: request_digest.finalize().into(),
        input_fingerprint: frozen.input_fingerprint,
    };
    Ok(PreparedGeometry {
        submission,
        frozen,
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
