//! Run and verify a local image-quality inspection for a registered sample.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_protocol::v1::{ImageQualityInput, ImageQualityPolicy, WorkerInputFile};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_manifest::{
    MANIFEST_FILE, TaskManifestScope, parse_stored_manifest_reference, verify_task_result_manifest,
};
use crate::dataset_query::RegisteredImage;
use crate::input_fingerprint::{deserialize_manifest_u32, freeze_input_manifest};
use crate::json::{JsonContracts, JsonProfile, strict_json};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{StoredTask, TaskOutcome, TaskStoreError, TaskSubmission, format_digest};
use crate::worker_runtime::{WorkerJob, WorkerOperation, WorkerRuntime, now_ms};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub use crate::worker_runtime::WorkerRuntimeError as ImageQualityError;

pub(crate) const CAPABILITY: &str = "image_quality.v1";
const OPERATION: &str = "IMAGE_QUALITY";
const DEADLINE: Duration = Duration::from_secs(30);
const RESULT_FILE: &str = "image-quality.json";
const JSON_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageQualityThresholds {
    #[serde(deserialize_with = "deserialize_manifest_u32")]
    pub dark_luma_max: u32,
    #[serde(deserialize_with = "deserialize_manifest_u32")]
    pub bright_luma_min: u32,
    pub exposure_fraction: f64,
    pub min_gradient_energy: f64,
}

impl ImageQualityThresholds {
    fn validate(self) -> Result<(), ImageQualityError> {
        if self.dark_luma_max >= self.bright_luma_min
            || self.bright_luma_min > 255
            || !(0.0 < self.exposure_fraction && self.exposure_fraction <= 1.0)
            || !self.min_gradient_energy.is_finite()
            || self.min_gradient_energy < 0.0
        {
            return Err(ImageQualityError::InvalidBinding);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ImageQualityRequest {
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub max_pixels: u64,
    pub thresholds: ImageQualityThresholds,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

pub struct ImageQualityJob {
    task: StoredTask,
    session: Uuid,
    worker_job: WorkerJob,
}

impl ImageQualityJob {
    pub fn task(&self) -> &StoredTask {
        &self.task
    }
}

pub struct ImageQualityRuntime {
    runtime: WorkerRuntime,
}

impl ImageQualityRuntime {
    pub async fn launch(
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, ImageQualityError> {
        Ok(Self {
            runtime: WorkerRuntime::launch(workspace, session, interpreter, entrypoint, CAPABILITY)
                .await?,
        })
    }

    pub fn admit(
        workspace: &WorkspaceThread,
        session: Uuid,
        request: ImageQualityRequest,
    ) -> Result<ImageQualityJob, ImageQualityError> {
        request.thresholds.validate()?;
        if request.max_pixels == 0 {
            return Err(ImageQualityError::InvalidBinding);
        }
        let RegisteredImage {
            asset_id,
            dataset_id,
            image,
            label_schema_fingerprint,
            ..
        } = workspace.registered_image(session, request.dataset_id, request.asset_id)?;
        let root = workspace.storage_root(session)?;
        let contracts = JsonContracts::new().map_err(ImageQualityError::TaskEvidence)?;
        let policy = request.thresholds;
        let frozen = freeze_input_manifest(&contracts, json!({
            "dataset_id": dataset_id.to_string(), "label_schema_digest": label_schema_fingerprint,
            "assets": [{
                "asset_id": asset_id.to_string(), "media_digest": image.sha256,
                "annotation_digest": null, "source_metadata_digest": null,
            }],
            "capability_id": CAPABILITY, "implementation_version": env!("CARGO_PKG_VERSION"),
            "model_identity": null,
            "resolved_config": {"max_pixels": request.max_pixels, "quality_policy": policy},
        })).map_err(ImageQualityError::Fingerprint)?;
        let manifest_sha = format_digest(&frozen.sha256);
        let relative = Path::new("artifacts/task-inputs").join(format!("{manifest_sha}.json"));
        let parameters = ImageQualityInput {
            image: Some(WorkerInputFile {
                path: path_text(&image.path)?,
                sha256: image.sha256,
            }),
            input_manifest: Some(WorkerInputFile {
                path: path_text(&root.join(&relative))?,
                sha256: manifest_sha,
            }),
            max_pixels: request.max_pixels,
            policy: Some(ImageQualityPolicy {
                dark_luma_max: policy.dark_luma_max,
                bright_luma_min: policy.bright_luma_min,
                exposure_fraction: policy.exposure_fraction,
                min_gradient_energy: policy.min_gradient_energy,
            }),
            ..Default::default()
        };
        publish_immutable(&root, &relative, &frozen.bytes)?;
        let mut digest = Sha256::new();
        digest.update(b"datasetdoctor.task.image-quality.v1\0");
        digest.update(frozen.input_fingerprint);
        let task = workspace.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: Some(dataset_id),
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: OPERATION.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: relative,
                request_fingerprint: digest.finalize().into(),
                input_fingerprint: frozen.input_fingerprint,
            },
            now_ms()?,
        )?;
        let worker_job = WorkerJob {
            task: task.task_id,
            input_fingerprint: frozen.input_fingerprint,
            manifest_sha: frozen.sha256,
            manifest_size: frozen.bytes.len() as u64,
            operation: WorkerOperation::ImageQuality(parameters),
        };
        Ok(ImageQualityJob {
            task,
            session,
            worker_job,
        })
    }

    pub async fn run(&mut self, job: ImageQualityJob) -> Result<TaskOutcome, ImageQualityError> {
        let session = self.runtime.session.workspace_session_id;
        if job.session != session {
            return Err(ImageQualityError::StaleWorkspace);
        }
        if job.task.workspace_session != session {
            return Ok(self.runtime.outcome(job.task.task_id).await?);
        }
        self.runtime.execute(job.worker_job, DEADLINE).await
    }

    pub async fn shutdown(self) -> Result<(), ImageQualityError> {
        self.runtime.shutdown().await
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImageQualityReport {
    pub input_fingerprint: String,
    pub image: QualityImage,
    pub policy: ImageQualityThresholds,
    pub measurements: Option<QualityMeasurements>,
    pub checks: Vec<QualityCheck>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualityImage {
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualityMeasurements {
    pub visible_pixels: u64,
    pub dark_fraction: Option<f64>,
    pub bright_fraction: Option<f64>,
    pub gradient_energy: Option<f64>,
    pub findings: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QualityCheck {
    pub check_id: String,
    pub execution_status: String,
    pub assessment: String,
    pub reason_code: Option<String>,
}

struct AcceptedQuality {
    root: PathBuf,
    attempt: Uuid,
    input: PathBuf,
    fingerprint: String,
    output: PathBuf,
    receipt: String,
}

pub fn read_image_quality(
    workspace: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    asset: Uuid,
    task: Uuid,
) -> Result<ImageQualityReport, ImageQualityError> {
    let accepted = workspace.workspace_action(session, move |owner| {
        owner.connection.query_row(
            "SELECT current_attempt_id,input_manifest_path,input_fingerprint,output_manifest_path,
                    json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
             FROM tasks WHERE task_id=?1 AND dataset_id=?2 AND operation_kind=?3
                    AND capability_id=?4 AND status='SUCCEEDED'",
            params![task.to_string(), dataset.to_string(), OPERATION, CAPABILITY],
            |row| Ok(AcceptedQuality {
                root: owner.root().to_owned(),
                attempt: row_uuid(row, 0)?,
                input: PathBuf::from(row.get::<_, String>(1)?),
                fingerprint: row.get(2)?,
                output: PathBuf::from(row.get::<_, String>(3)?),
                receipt: row.get(4)?,
            }),
        ).optional().map_err(TaskStoreError::Database).map_err(WorkspaceThreadError::Task)?
         .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
    })?;
    let invalid = || ImageQualityError::ResultUnconfirmed;
    let contracts = JsonContracts::new().map_err(ImageQualityError::TaskEvidence)?;
    let input_bytes = read_scoped(&accepted.root, &accepted.input, JSON_LIMIT)?;
    let input = contracts
        .validate(JsonProfile::InputManifest, &input_bytes)
        .map_err(evidence)?;
    let frozen =
        freeze_input_manifest(&contracts, input.clone()).map_err(ImageQualityError::Fingerprint)?;
    let Some([sample]) = input["assets"].as_array().map(Vec::as_slice) else {
        return Err(invalid());
    };
    if frozen.bytes != input_bytes
        || format_digest(&frozen.input_fingerprint) != accepted.fingerprint
        || input["dataset_id"] != dataset.to_string()
        || input["capability_id"] != CAPABILITY
        || sample["asset_id"] != asset.to_string()
    {
        return Err(invalid());
    }

    let relative = PathBuf::from(format!("artifacts/tasks/{task}/{}", accepted.attempt));
    if accepted.output != relative.join(MANIFEST_FILE) {
        return Err(invalid());
    }
    let directory = accepted.root.join(relative);
    if directory.canonicalize()? != directory {
        return Err(invalid());
    }
    let receipt = parse_stored_manifest_reference(&accepted.receipt, task).map_err(evidence)?;
    let scope = TaskManifestScope {
        task,
        attempt: accepted.attempt,
        input_fingerprint: frozen.input_fingerprint,
    };
    let verified =
        verify_task_result_manifest(&directory, &contracts, &scope, &receipt).map_err(evidence)?;
    let [entry] = verified.entries() else {
        return Err(invalid());
    };
    if entry.relative_path != RESULT_FILE
        || entry.kind != "CHECK_RESULT"
        || entry.media_type != "application/json"
        || entry.size_bytes > JSON_LIMIT
    {
        return Err(invalid());
    }
    let bytes = read_scoped(&directory, Path::new(RESULT_FILE), entry.size_bytes)?;
    if bytes.len() as u64 != entry.size_bytes
        || format_digest(&Sha256::digest(&bytes).into()) != entry.sha256
    {
        return Err(invalid());
    }
    let report: ImageQualityReport =
        serde_json::from_value(strict_json(&bytes).map_err(evidence)?).map_err(evidence)?;
    let policy: ImageQualityThresholds =
        serde_json::from_value(input["resolved_config"]["quality_policy"].clone())
            .map_err(evidence)?;
    if report.input_fingerprint != accepted.fingerprint
        || sample["media_digest"].as_str() != Some(report.image.sha256.as_str())
        || report.policy != policy
    {
        return Err(invalid());
    }
    Ok(report)
}

fn read_scoped(root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>, ImageQualityError> {
    let mut bytes = Vec::new();
    open_scoped_file(root, path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(ImageQualityError::ResultUnconfirmed);
    }
    Ok(bytes)
}

fn path_text(path: &Path) -> Result<String, ImageQualityError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(ImageQualityError::InvalidBinding)
}

fn evidence(error: impl std::error::Error + Send + Sync + 'static) -> ImageQualityError {
    ImageQualityError::TaskEvidence(Box::new(error))
}
