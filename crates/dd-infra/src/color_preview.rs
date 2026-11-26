//! Admit and run color previews against a registered image in the active Workspace.

mod result;
pub use result::VerifiedColorPreview;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dd_protocol::v1::{
    ColorHueGate, ColorMethod, ColorOperation, ColorPoint, ColorPolygon, ColorPreviewInput,
    ColorRectangle, WorkerInputFile, color_preview_input::Region,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Number, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dataset_query::RegisteredInputFile;
use crate::input_fingerprint::{
    FrozenInputManifest, deserialize_manifest_u32, freeze_input_manifest, manifest_u32,
};
use crate::json::JsonContracts;
use crate::media_display::{MediaDisplayService, MediaLocator};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskOutcome, TaskStoreError, TaskSubmission, format_digest};
use crate::worker_runtime::{WorkerJob, WorkerOperation, WorkerRuntime, now_ms};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub use crate::worker_runtime::WorkerRuntimeError as ColorPreviewError;

pub(crate) const CAPABILITY: &str = "color_preview.v1";
const KIND: &str = "COLOR_PREVIEW";
const MAX_PIXELS: u32 = 100_000_000;
pub(crate) const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RepairColorMethod {
    HueRemap,
    SolidFill,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ColorRegion {
    #[serde(rename = "RECT")]
    Rectangle { bbox_xywh: [f64; 4] },
    #[serde(rename = "POLYGON")]
    Polygon { points_xy: Vec<[f64; 2]> },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ColorGate {
    pub hue_min_deg: f64,
    pub hue_max_deg: f64,
    pub wraps_zero: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ColorOptions {
    pub method: RepairColorMethod,
    #[serde(deserialize_with = "rgb_components")]
    pub target_rgb: [u8; 3],
    pub strength: f64,
    pub feather_px: f64,
    pub source_color_gate: Option<ColorGate>,
}

#[derive(Clone, Debug)]
pub struct ColorPreviewSubmission {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub base_image_digest: String,
    pub region: ColorRegion,
    pub options: ColorOptions,
}

pub struct ColorPreviewJob {
    session: Uuid,
    worker_job: WorkerJob,
}

impl ColorPreviewJob {
    pub fn task_id(&self) -> Uuid {
        self.worker_job.task
    }
}

#[derive(Debug)]
pub struct ColorPreviewSnapshot {
    pub task_id: Uuid,
    pub accepted_at_ms: i64,
    pub status: String,
    pub error_code: Option<String>,
    pub preview: Option<VerifiedColorPreview>,
}

pub struct ColorPreviewService {
    owner: Arc<WorkspaceThread>,
    pub(crate) contracts: JsonContracts,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PreviewConfig {
    asset_revision: String,
    #[serde(deserialize_with = "deserialize_manifest_u32")]
    max_pixels: u32,
    region: ColorRegion,
    operation: ColorOptions,
}

impl ColorPreviewService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, ColorPreviewError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(ColorPreviewError::TaskEvidence)?,
        })
    }

    pub fn submit(
        &self,
        session: Uuid,
        request: ColorPreviewSubmission,
        now: i64,
    ) -> Result<(ColorPreviewSnapshot, Option<ColorPreviewJob>), ColorPreviewError> {
        if request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || request.dataset_id.is_nil()
            || request.asset_id.is_nil()
            || crate::task_store::parse_digest(&request.base_image_digest).is_none()
            || !request.region.valid()
            || !request.options.valid()
        {
            return Err(ColorPreviewError::InvalidBinding);
        }
        let _guard = self.owner.lock_final_operations()?;
        let dataset = request.dataset_id;
        let asset = request.asset_id;
        let fingerprint: [u8; 32] = Sha256::digest(
            json!([
                CAPABILITY,
                dataset,
                asset,
                request.base_image_digest,
                &request.region,
                &request.options,
            ])
            .to_string()
            .as_bytes(),
        )
        .into();
        let key = request.idempotency_key.to_string();
        let existing = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    "SELECT task_id, request_fingerprint FROM tasks
                 WHERE operation_kind=?1 AND idempotency_key=?2",
                    params![KIND, key],
                    |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(database)
        })?;
        if let Some((task, recorded)) = existing {
            if recorded != format_digest(&fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return Ok((self.snapshot(session, task)?, None));
        }
        let (root, image, revision) = self.owner.workspace_action(session, move |workspace| {
            let image = workspace
                .registered_image(dataset, asset)
                .map_err(WorkspaceThreadError::Dataset)?;
            let revision = workspace
                .connection
                .query_row(
                    "SELECT row_revision FROM assets WHERE asset_id=?1",
                    [asset.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(database)?;
            Ok((workspace.root().to_owned(), image, revision))
        })?;
        if revision < 0
            || image.image.sha256 != request.base_image_digest
            || !image.image.live_digest(MAX_IMAGE_BYTES)?
        {
            return Err(ColorPreviewError::InvalidBinding);
        }
        let config = PreviewConfig {
            asset_revision: revision.to_string(),
            max_pixels: MAX_PIXELS,
            region: request.region,
            operation: request.options,
        };
        let frozen = freeze_input_manifest(
            &self.contracts,
            json!({
                "dataset_id": dataset, "label_schema_digest": image.label_schema_fingerprint,
                "assets": [{"asset_id":asset,"media_digest":image.image.sha256,
                    "annotation_digest":image.annotation.as_ref().map(|file|&file.sha256),
                    "source_metadata_digest":null}],
                "capability_id":CAPABILITY,"implementation_version":env!("CARGO_PKG_VERSION"),
                "model_identity":null,"resolved_config":config,
            }),
        )
        .map_err(ColorPreviewError::Fingerprint)?;
        if frozen.bytes.len() as u64 > MAX_INPUT_BYTES {
            return Err(ColorPreviewError::InvalidBinding);
        }
        let relative = input_path(&frozen);
        publish_immutable(&root, &relative, &frozen.bytes)?;
        let input = ColorPreviewInput {
            image: Some(locator(&image.image.path, image.image.sha256)?),
            input_manifest: Some(locator(
                &root.join(&relative),
                format_digest(&frozen.sha256),
            )?),
            output_directory: String::new(),
            max_pixels: u64::from(config.max_pixels),
            region: Some(config.region.wire()),
            operation: Some(config.operation.wire()),
        };
        let task = self.owner.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: Some(dataset),
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: KIND.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: relative,
                request_fingerprint: fingerprint,
                input_fingerprint: frozen.input_fingerprint,
            },
            now,
        )?;
        Ok((
            ColorPreviewSnapshot {
                task_id: task.task_id,
                accepted_at_ms: task.created_at_ms,
                status: task.status,
                error_code: None,
                preview: None,
            },
            Some(ColorPreviewJob {
                session,
                worker_job: WorkerJob {
                    task: task.task_id,
                    input_fingerprint: frozen.input_fingerprint,
                    manifest_sha: frozen.sha256,
                    manifest_size: frozen.bytes.len() as u64,
                    operation: WorkerOperation::ColorPreview(input),
                },
            }),
        ))
    }

    pub fn snapshot(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ColorPreviewSnapshot, ColorPreviewError> {
        let now = now_ms()?;
        let record = self.owner.workspace_action(session, move |workspace| {
            workspace.connection.execute(
                "UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',finished_at_ms=?4,
                 updated_at_ms=?4,row_revision=row_revision+1
                 WHERE task_id=?1 AND capability_id=?2 AND workspace_session_id<>?3
                   AND status IN ('QUEUED','DISPATCHING','RUNNING')",
                params![task.to_string(), CAPABILITY, session.to_string(), now],
            ).map_err(database)?;
            workspace.connection.query_row(
                "SELECT dataset_id,created_at_ms,status,error_code,input_manifest_path,input_fingerprint,
                    current_attempt_id,output_manifest_path,
                    json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
                 FROM tasks WHERE task_id=?1 AND capability_id=?2 AND operation_kind=?3",
                params![task.to_string(),CAPABILITY,KIND],
                |row| Ok(StoredPreview {
                    root: workspace.root().to_owned(), dataset:row_uuid(row,0)?, accepted_at:row.get(1)?,
                    status:row.get(2)?, error:row.get(3)?, input_path:PathBuf::from(row.get::<_, String>(4)?),
                    input_fingerprint:row.get(5)?, attempt:row.get(6)?, output:row.get(7)?, reference:row.get(8)?,
                }),
            ).optional().map_err(database)?
                .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
        })?;
        let preview = if record.status == "SUCCEEDED" {
            Some(result::read_preview(task, &record, &self.contracts)?)
        } else {
            None
        };
        Ok(ColorPreviewSnapshot {
            task_id: task,
            accepted_at_ms: record.accepted_at,
            status: record.status,
            error_code: record.error,
            preview,
        })
    }

    pub fn image_locator(
        &self,
        media: &MediaDisplayService,
        session: Uuid,
        task: Uuid,
        artifact: Uuid,
        now: Instant,
    ) -> Result<MediaLocator, ColorPreviewError> {
        let preview = self
            .snapshot(session, task)?
            .preview
            .ok_or(ColorPreviewError::InvalidBinding)?;
        let (reference, path) = if preview.candidate_ref.artifact_id == artifact {
            (preview.candidate_ref, preview.candidate_path)
        } else if preview.mask_ref.artifact_id == artifact {
            (preview.mask_ref, preview.mask_path)
        } else {
            return Err(ColorPreviewError::InvalidBinding);
        };
        let root = self.owner.storage_root(session)?;
        media
            .issue_verified_image(
                session,
                RegisteredInputFile {
                    path: root.join(path),
                    sha256: reference.sha256,
                },
                now,
            )
            .map_err(|error| ColorPreviewError::TaskEvidence(Box::new(error)))
    }
}

struct StoredPreview {
    root: PathBuf,
    dataset: Uuid,
    accepted_at: i64,
    status: String,
    error: Option<String>,
    input_path: PathBuf,
    input_fingerprint: String,
    attempt: Option<String>,
    output: Option<String>,
    reference: Option<String>,
}

pub struct ColorPreviewRuntime {
    runtime: WorkerRuntime,
}

impl ColorPreviewRuntime {
    pub fn record_unavailable(
        owner: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        startup_failed: bool,
    ) -> Result<(), ColorPreviewError> {
        WorkerRuntime::record_unavailable(owner, session, task, CAPABILITY, startup_failed)
    }

    pub async fn launch(
        owner: Arc<WorkspaceThread>,
        session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, ColorPreviewError> {
        Ok(Self {
            runtime: WorkerRuntime::launch(owner, session, interpreter, entrypoint, CAPABILITY)
                .await?,
        })
    }

    pub async fn run(&mut self, job: ColorPreviewJob) -> Result<TaskOutcome, ColorPreviewError> {
        if job.session != self.runtime.session.workspace_session_id {
            return Err(ColorPreviewError::StaleWorkspace);
        }
        self.runtime
            .execute(job.worker_job, Duration::from_secs(60))
            .await
    }

    pub async fn shutdown(self) -> Result<(), ColorPreviewError> {
        self.runtime.shutdown().await
    }
    pub async fn terminate(self) -> Result<(), ColorPreviewError> {
        self.runtime.terminate().await
    }
}

impl ColorRegion {
    fn valid(&self) -> bool {
        match self {
            Self::Rectangle {
                bbox_xywh: [x, y, width, height],
            } => {
                x.is_finite()
                    && y.is_finite()
                    && width.is_finite()
                    && height.is_finite()
                    && *x >= 0.0
                    && *y >= 0.0
                    && *width > 0.0
                    && *height > 0.0
            }
            Self::Polygon { points_xy } => {
                points_xy.len() >= 3
                    && points_xy
                        .iter()
                        .flatten()
                        .all(|value| value.is_finite() && *value >= 0.0)
            }
        }
    }

    fn wire(self) -> Region {
        match self {
            Self::Rectangle {
                bbox_xywh: [x, y, width, height],
            } => Region::Rectangle(ColorRectangle {
                origin: Some(ColorPoint { x, y }),
                width,
                height,
            }),
            Self::Polygon { points_xy } => Region::Polygon(ColorPolygon {
                points: points_xy
                    .into_iter()
                    .map(|[x, y]| ColorPoint { x, y })
                    .collect(),
            }),
        }
    }
}

impl ColorOptions {
    fn valid(&self) -> bool {
        (0.0..=1.0).contains(&self.strength)
            && (0.0..=64.0).contains(&self.feather_px)
            && self.source_color_gate.as_ref().is_none_or(|gate| {
                (0.0..360.0).contains(&gate.hue_min_deg)
                    && (0.0..360.0).contains(&gate.hue_max_deg)
                    && gate.wraps_zero == (gate.hue_min_deg > gate.hue_max_deg)
            })
    }

    fn wire(self) -> ColorOperation {
        ColorOperation {
            method: match self.method {
                RepairColorMethod::HueRemap => ColorMethod::HueRemap,
                RepairColorMethod::SolidFill => ColorMethod::SolidFill,
            }
            .into(),
            target_rgb: self.target_rgb.into_iter().map(u32::from).collect(),
            strength: Some(self.strength),
            feather_px: self.feather_px,
            source_color_gate: self.source_color_gate.map(|gate| ColorHueGate {
                hue_min_deg: gate.hue_min_deg,
                hue_max_deg: gate.hue_max_deg,
                wraps_zero: gate.wraps_zero,
            }),
        }
    }
}

fn rgb_components<'de, D: serde::Deserializer<'de>>(input: D) -> Result<[u8; 3], D::Error> {
    let [red, green, blue] = <[Number; 3]>::deserialize(input)?;
    let component = |number| {
        manifest_u32(number)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| serde::de::Error::custom("Invalid sRGB component"))
    };
    Ok([component(&red)?, component(&green)?, component(&blue)?])
}

fn input_path(input: &FrozenInputManifest) -> PathBuf {
    PathBuf::from("artifacts/task-inputs").join(format!("{}.json", format_digest(&input.sha256)))
}

fn locator(path: &Path, sha256: String) -> Result<WorkerInputFile, ColorPreviewError> {
    let path = path
        .to_str()
        .ok_or(ColorPreviewError::InvalidBinding)?
        .to_owned();
    Ok(WorkerInputFile { path, sha256 })
}

pub(crate) fn read_scoped(
    root: &Path,
    path: &Path,
    limit: u64,
) -> Result<Vec<u8>, ColorPreviewError> {
    let mut bytes = Vec::new();
    open_scoped_file(root, path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(ColorPreviewError::ResultUnconfirmed);
    }
    Ok(bytes)
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
