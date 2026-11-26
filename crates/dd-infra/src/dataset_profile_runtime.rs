//! Freeze registered Dataset members and profile them through the local Worker.

mod result;

pub use crate::worker_runtime::WorkerRuntimeError as DatasetProfileError;
pub use result::{DatasetProfile, ProfileFileError, ProfileResolution, read_dataset_profile};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_protocol::v1::{DatasetProfileAsset, DatasetProfileInput, WorkerInputFile};
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dataset_query::{DatasetQueryError, RegisteredInputFile};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::JsonContracts;
use crate::paths::publish_immutable;
use crate::task_store::{StoredTask, TaskOutcome, TaskStoreError, TaskSubmission, format_digest};
use crate::worker_runtime::{WorkerJob, WorkerOperation, WorkerRuntime, now_ms};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub(crate) const CAPABILITY: &str = "dataset_profile.v1";
const OPERATION: &str = "DATASET_PROFILE";
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct DatasetProfileRequest {
    pub dataset_id: Uuid,
    pub asset_ids: Vec<Uuid>,
    pub max_pixels: u32,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

pub struct DatasetProfileJob {
    session: Uuid,
    task: StoredTask,
    worker_job: WorkerJob,
}

impl DatasetProfileJob {
    pub fn task(&self) -> &StoredTask {
        &self.task
    }
}

pub struct DatasetProfileRuntime {
    runtime: WorkerRuntime,
}

impl DatasetProfileRuntime {
    pub async fn launch(
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, DatasetProfileError> {
        let now = now_ms()?;
        let owner = Arc::clone(&workspace);
        tokio::task::spawn_blocking(move || {
            owner.workspace_action(session, move |opened| {
                opened
                    .connection
                    .execute(
                        "UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',
                    finished_at_ms=?4,updated_at_ms=?4,row_revision=row_revision+1
                 WHERE operation_kind=?1 AND capability_id=?2 AND workspace_session_id<>?3
                    AND status IN ('QUEUED','DISPATCHING','RUNNING','CANCELLING')",
                        params![OPERATION, CAPABILITY, session.to_string(), now],
                    )
                    .map_err(TaskStoreError::Database)
                    .map_err(WorkspaceThreadError::Task)?;
                Ok(())
            })
        })
        .await
        .map_err(|_| DatasetProfileError::CoreStopped)??;
        let runtime =
            WorkerRuntime::launch(workspace, session, interpreter, entrypoint, CAPABILITY).await?;
        Ok(Self { runtime })
    }

    pub fn admit(
        workspace: &WorkspaceThread,
        session: Uuid,
        mut request: DatasetProfileRequest,
    ) -> Result<DatasetProfileJob, DatasetProfileError> {
        request.asset_ids.sort_unstable();
        if request.max_pixels == 0 || request.asset_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DatasetProfileError::InvalidBinding);
        }
        let dataset_id = request.dataset_id;
        let (root, categories, registered) = workspace.workspace_action(session, move |owner| {
            let detail = owner
                .dataset_detail(dataset_id)
                .map_err(WorkspaceThreadError::Dataset)?;
            let mut membership = owner.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM dataset_assets WHERE dataset_id=?1 AND asset_id=?2)",
            ).map_err(DatasetQueryError::from).map_err(WorkspaceThreadError::Dataset)?;
            let mut registered = Vec::with_capacity(request.asset_ids.len());
            let dataset = dataset_id.to_string();
            for asset in request.asset_ids {
                let member: bool = membership
                    .query_row(params![&dataset, asset.to_string()], |row| row.get(0))
                    .map_err(DatasetQueryError::from)
                    .map_err(WorkspaceThreadError::Dataset)?;
                if !member {
                    return Err(WorkspaceThreadError::Dataset(
                        DatasetQueryError::UnknownAsset,
                    ));
                }
                registered.push(
                    owner
                        .registered_image(dataset_id, asset)
                        .map_err(WorkspaceThreadError::Dataset)?,
                );
            }
            Ok((
                owner.root().to_owned(),
                detail.summary.schema_fingerprint,
                registered,
            ))
        })?;
        let contracts = JsonContracts::new().map_err(|_| DatasetProfileError::CoreStopped)?;
        let manifest_assets: Vec<_> = registered
            .iter()
            .map(|entry| {
                json!({
                    "asset_id": entry.asset_id, "media_digest": entry.image.sha256,
                    "annotation_digest": entry.annotation.as_ref().map(|file| &file.sha256),
                    "source_metadata_digest": null,
                })
            })
            .collect();
        let frozen = freeze_input_manifest(
            &contracts,
            json!({
                "dataset_id": dataset_id, "label_schema_digest": categories,
                "assets": manifest_assets, "resolved_config": {"max_pixels": request.max_pixels},
                "capability_id": CAPABILITY, "implementation_version": env!("CARGO_PKG_VERSION"),
                "model_identity": null,
            }),
        )
        .map_err(DatasetProfileError::Fingerprint)?;
        let sha256 = format_digest(&frozen.sha256);
        let manifest_path = Path::new("artifacts/task-inputs").join(format!("{sha256}.json"));
        let assets = registered
            .into_iter()
            .map(|entry| {
                Ok(DatasetProfileAsset {
                    asset_id: entry.asset_id.to_string(),
                    image: Some(locator(entry.image)?),
                    annotation: entry.annotation.map(locator).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, DatasetProfileError>>()?;
        let input_manifest = locator(RegisteredInputFile {
            path: root.join(&manifest_path),
            sha256,
        })?;
        publish_immutable(&root, &manifest_path, &frozen.bytes)?;
        let mut request_fingerprint = Sha256::new();
        request_fingerprint.update(b"datasetdoctor.task.dataset-profile.v1\0");
        request_fingerprint.update(frozen.input_fingerprint);
        let task = workspace.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: Some(dataset_id),
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: OPERATION.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: manifest_path,
                request_fingerprint: request_fingerprint.finalize().into(),
                input_fingerprint: frozen.input_fingerprint,
            },
            now_ms()?,
        )?;
        let worker_job = WorkerJob {
            task: task.task_id,
            input_fingerprint: frozen.input_fingerprint,
            manifest_sha: frozen.sha256,
            manifest_size: frozen.bytes.len() as u64,
            operation: WorkerOperation::DatasetProfile(DatasetProfileInput {
                assets,
                input_manifest: Some(input_manifest),
                max_pixels: u64::from(request.max_pixels),
                ..Default::default()
            }),
        };
        Ok(DatasetProfileJob {
            session,
            task,
            worker_job,
        })
    }

    pub async fn run(
        &mut self,
        job: DatasetProfileJob,
    ) -> Result<TaskOutcome, DatasetProfileError> {
        let session = self.runtime.session.workspace_session_id;
        if job.session != session {
            return Err(DatasetProfileError::StaleWorkspace);
        }
        if job.task.workspace_session != session {
            return self.runtime.outcome(job.task.task_id).await;
        }
        self.runtime.execute(job.worker_job, DEADLINE).await
    }

    pub async fn shutdown(self) -> Result<(), DatasetProfileError> {
        self.runtime.shutdown().await
    }
}

fn locator(file: RegisteredInputFile) -> Result<WorkerInputFile, DatasetProfileError> {
    let path = file
        .path
        .to_str()
        .ok_or(DatasetProfileError::InvalidBinding)?
        .to_owned();
    Ok(WorkerInputFile {
        path,
        sha256: file.sha256,
    })
}
