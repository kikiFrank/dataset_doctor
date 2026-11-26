//! Persist and execute exact display-frame requests from registered Workspace videos.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_domain::video::FrameRef;
pub use dd_domain::video::SeekPolicy;
use dd_protocol::v1::{FrameResolveInput, WorkerInputFile};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_manifest::{MANIFEST_FILE, TaskManifestScope, parse_stored_manifest_reference};
use crate::input_fingerprint::{FrozenInputManifest, freeze_input_manifest};
use crate::json::{JsonContracts, JsonProfile};
use crate::paths::{
    is_portable_relative_file, open_registered_file, open_scoped_file, publish_immutable,
};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskOutcome, TaskStoreError, TaskSubmission, format_digest, parse_digest};
use crate::video_index::{ResolvedVideoFrame, read_resolved_video_frame};
use crate::video_query::{VideoSourceAvailability, registered_video_detail};
use crate::video_runtime::read_registered_video_index;
use crate::worker_runtime::{
    WorkerJob, WorkerOperation, WorkerRuntime, WorkerRuntimeError, now_ms,
};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub use crate::worker_runtime::WorkerRuntimeError as FrameResolveError;

pub(crate) const CAPABILITY: &str = "frame_resolve.v1";
const KIND: &str = "FRAME_RESOLVE";
const INPUT_LIMIT: u64 = 128 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum FrameTarget {
    Index(u64),
    TimestampMs(u64),
}

#[derive(Clone, Copy)]
pub struct FrameSubmission {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub video_id: Uuid,
    pub target: FrameTarget,
    pub policy: SeekPolicy,
}

pub struct FrameResolveJob {
    task_id: Uuid,
}

impl FrameResolveJob {
    pub fn task_id(&self) -> Uuid {
        self.task_id
    }
}

pub struct FrameTaskSnapshot {
    pub task_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub accepted_at_ms: i64,
    pub status: String,
    pub error_code: Option<String>,
    pub frame: Option<ResolvedVideoFrame>,
}

pub struct FrameService {
    owner: Arc<WorkspaceThread>,
    contracts: Arc<JsonContracts>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrameConfiguration {
    video_id: Uuid,
    stream_index: u32,
    frame_index: String,
    raw_pts: Option<String>,
    pts_us: Option<String>,
    width: u32,
    height: u32,
}

impl FrameConfiguration {
    fn from_frame(frame: FrameRef) -> Self {
        Self {
            video_id: frame.source_video_id,
            stream_index: frame.stream_index,
            frame_index: frame.frame_index.to_string(),
            raw_pts: frame.raw_pts.map(|value| value.to_string()),
            pts_us: frame.pts_us.map(|value| value.to_string()),
            width: frame.width,
            height: frame.height,
        }
    }

    fn frame(&self, source_digest: [u8; 32]) -> Result<FrameRef, WorkerRuntimeError> {
        let frame_index = self
            .frame_index
            .parse::<u64>()
            .map_err(|_| WorkerRuntimeError::InvalidBinding)?;
        if frame_index.to_string() != self.frame_index || self.width == 0 || self.height == 0 {
            return Err(WorkerRuntimeError::InvalidBinding);
        }
        Ok(FrameRef {
            source_video_id: self.video_id,
            source_digest,
            stream_index: self.stream_index,
            frame_index,
            raw_pts: self.raw_pts.as_deref().map(signed_decimal).transpose()?,
            pts_us: self.pts_us.as_deref().map(signed_decimal).transpose()?,
            width: self.width,
            height: self.height,
        })
    }
}

impl FrameService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, WorkerRuntimeError> {
        Ok(Self {
            owner,
            contracts: Arc::new(JsonContracts::new().map_err(WorkerRuntimeError::TaskEvidence)?),
        })
    }

    pub fn submit(
        &self,
        session: Uuid,
        request: FrameSubmission,
        accepted_at_ms: i64,
    ) -> Result<(FrameTaskSnapshot, Option<FrameResolveJob>), WorkerRuntimeError> {
        if request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || request.video_id.is_nil()
        {
            return Err(WorkerRuntimeError::InvalidBinding);
        }
        let _guard = self.owner.lock_final_operations()?;
        let request_fingerprint = request_digest(request);
        let existing = self.owner.workspace_action(session, move |workspace| {
            workspace.connection.query_row(
                "SELECT task_id,request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
                params![KIND, request.idempotency_key.to_string()],
                |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
            ).optional().map_err(database)
        })?;
        if let Some((task_id, fingerprint)) = existing {
            if fingerprint != format_digest(&request_fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return Ok((self.snapshot(session, task_id)?, None));
        }

        let detail = registered_video_detail(&self.owner, session, request.video_id)?;
        if detail.source_availability != VideoSourceAvailability::Online
            || detail.probe_status.as_deref() != Some("SUCCEEDED")
        {
            return Err(WorkerRuntimeError::InvalidBinding);
        }
        let digest =
            parse_digest(&detail.source_digest).ok_or(WorkerRuntimeError::InvalidBinding)?;
        let probe = detail
            .probe_receipt
            .ok_or(WorkerRuntimeError::InvalidBinding)?;
        let index = read_registered_video_index(&self.owner, session, probe)?;
        let frame = match request.target {
            FrameTarget::Index(index_value) => index.resolve_index(digest, index_value),
            FrameTarget::TimestampMs(timestamp) => {
                let micros = timestamp
                    .checked_mul(1000)
                    .and_then(|value| i64::try_from(value).ok())
                    .ok_or(WorkerRuntimeError::InvalidBinding)?;
                index.resolve_time(digest, micros, request.policy)
            }
        }
        .map_err(|error| WorkerRuntimeError::TaskEvidence(Box::new(error)))?;
        let frozen = freeze_input_manifest(
            &self.contracts,
            json!({
                "dataset_id": null, "label_schema_digest": null,
                "assets": [{"asset_id": request.video_id, "media_digest": format_digest(&digest),
                    "annotation_digest": null, "source_metadata_digest": null}],
                "capability_id": CAPABILITY, "implementation_version": env!("CARGO_PKG_VERSION"),
                "model_identity": null, "resolved_config": FrameConfiguration::from_frame(frame),
            }),
        )
        .map_err(WorkerRuntimeError::Fingerprint)?;
        let fingerprint = format_digest(&frozen.input_fingerprint);
        let cached = self.owner.workspace_action(session, move |workspace| {
            workspace.connection.query_row(
                "SELECT task_id FROM tasks WHERE capability_id=?1 AND input_fingerprint=?2 AND status='SUCCEEDED'
                 ORDER BY finished_at_ms DESC, rowid DESC LIMIT 1",
                params![CAPABILITY, fingerprint], |row| row_uuid(row, 0),
            ).optional().map_err(database)
        })?;
        if let Some(task_id) = cached {
            return Ok((self.snapshot(session, task_id)?, None));
        }
        let root = self.owner.storage_root(session)?;
        let path = Path::new("artifacts/task-inputs")
            .join(format!("{}.json", format_digest(&frozen.sha256)));
        publish_immutable(&root, &path, &frozen.bytes)?;
        let task = self.owner.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: None,
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: KIND.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: path,
                request_fingerprint,
                input_fingerprint: frozen.input_fingerprint,
            },
            accepted_at_ms,
        )?;
        Ok((
            FrameTaskSnapshot {
                task_id: task.task_id,
                attempt_id: None,
                accepted_at_ms: task.created_at_ms,
                status: task.status,
                error_code: None,
                frame: None,
            },
            Some(FrameResolveJob {
                task_id: task.task_id,
            }),
        ))
    }

    pub fn image_locator(
        &self,
        media: &crate::media_display::MediaDisplayService,
        session: Uuid,
        task: Uuid,
        artifact: Uuid,
        now: std::time::Instant,
    ) -> Result<crate::media_display::MediaLocator, FrameResolveError> {
        let resolved = self
            .snapshot(session, task)?
            .frame
            .ok_or(FrameResolveError::ResultUnconfirmed)?;
        if resolved.reference.artifact_id != artifact.to_string() {
            return Err(FrameResolveError::InvalidBinding);
        }
        let root = self.owner.storage_root(session)?;
        media
            .issue_verified_image(
                session,
                crate::dataset_query::RegisteredInputFile {
                    path: root.join(resolved.relative_path),
                    sha256: resolved.reference.sha256,
                },
                now,
            )
            .map_err(|error| FrameResolveError::TaskEvidence(Box::new(error)))
    }

    pub fn snapshot(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<FrameTaskSnapshot, WorkerRuntimeError> {
        let now = now_ms()?;
        self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .execute(
                    "UPDATE tasks SET status='INTERRUPTED', error_code='SESSION_ENDED',
                    finished_at_ms=?4,updated_at_ms=?4,row_revision=row_revision+1
                 WHERE task_id=?1 AND capability_id=?2 AND workspace_session_id<>?3
                   AND status IN ('QUEUED','DISPATCHING','RUNNING')",
                    params![task.to_string(), CAPABILITY, session.to_string(), now],
                )
                .map_err(database)?;
            Ok(())
        })?;
        let loaded = load_task(&self.owner, session, task, &self.contracts)?;
        let frame = if loaded.status == "SUCCEEDED" {
            let attempt = loaded.attempt.ok_or(WorkerRuntimeError::InvalidBinding)?;
            let relative = PathBuf::from(format!("artifacts/tasks/{task}/{attempt}"));
            if loaded.output.as_deref().map(Path::new)
                != Some(relative.join(MANIFEST_FILE).as_path())
            {
                return Err(WorkerRuntimeError::InvalidBinding);
            }
            let directory = loaded.root.join(&relative);
            if directory.canonicalize()? != directory {
                return Err(WorkerRuntimeError::InvalidBinding);
            }
            let reference = parse_stored_manifest_reference(
                loaded
                    .result_reference
                    .as_deref()
                    .ok_or(WorkerRuntimeError::InvalidBinding)?,
                task,
            )
            .map_err(|error| WorkerRuntimeError::TaskEvidence(Box::new(error)))?;
            let mut resolved = read_resolved_video_frame(
                &directory,
                &reference,
                &TaskManifestScope {
                    task,
                    attempt,
                    input_fingerprint: loaded.frozen.input_fingerprint,
                },
                loaded.frame,
            )
            .map_err(WorkerRuntimeError::TaskEvidence)?;
            resolved.relative_path = relative.join(resolved.relative_path);
            Some(resolved)
        } else {
            None
        };
        Ok(FrameTaskSnapshot {
            task_id: task,
            attempt_id: loaded.attempt,
            accepted_at_ms: loaded.created_at_ms,
            status: loaded.status,
            error_code: loaded.error_code,
            frame,
        })
    }
}

pub struct FrameResolveRuntime {
    runtime: WorkerRuntime,
    contracts: Arc<JsonContracts>,
}

impl FrameResolveRuntime {
    pub fn task_outcome(
        owner: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
    ) -> Result<TaskOutcome, WorkspaceThreadError> {
        WorkerRuntime::task_outcome(owner, session, task, CAPABILITY)
    }

    pub fn record_unavailable(
        owner: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        startup_failed: bool,
    ) -> Result<(), FrameResolveError> {
        WorkerRuntime::record_unavailable(owner, session, task, CAPABILITY, startup_failed)
    }

    pub async fn launch(
        owner: Arc<WorkspaceThread>,
        session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, WorkerRuntimeError> {
        Ok(Self {
            runtime: WorkerRuntime::launch(owner, session, interpreter, entrypoint, CAPABILITY)
                .await?,
            contracts: Arc::new(JsonContracts::new().map_err(WorkerRuntimeError::TaskEvidence)?),
        })
    }

    pub async fn run(&mut self, job: FrameResolveJob) -> Result<TaskOutcome, WorkerRuntimeError> {
        let owner = Arc::clone(&self.runtime.workspace);
        let session = self.runtime.session.workspace_session_id;
        let contracts = Arc::clone(&self.contracts);
        let (loaded, source) = tokio::task::spawn_blocking(move || {
            let loaded = load_task(&owner, session, job.task_id, &contracts)?;
            let source = if loaded.status == "QUEUED" {
                Some(registered_source(
                    &owner,
                    session,
                    loaded.frame.source_video_id,
                    loaded.frame.source_digest,
                )?)
            } else {
                None
            };
            Ok::<_, WorkerRuntimeError>((loaded, source))
        })
        .await
        .map_err(|_| WorkerRuntimeError::CoreStopped)??;
        if loaded.status != "QUEUED" {
            return self.runtime.outcome(job.task_id).await;
        }
        if loaded.session != session {
            return Err(WorkerRuntimeError::StaleWorkspace);
        }
        let source = source.ok_or(WorkerRuntimeError::InvalidBinding)?;
        let source_for_check = source.clone();
        let expected = loaded.frame.source_digest;
        let freshness = tokio::task::spawn_blocking(move || {
            let mut bytes = open_registered_file(&source_for_check)?;
            let mut hash = Sha256::new();
            io::copy(&mut bytes, &mut hash)?;
            Ok::<_, io::Error>(<[u8; 32]>::from(hash.finalize()) == expected)
        })
        .await
        .map_err(|_| WorkerRuntimeError::CoreStopped)?;
        if !matches!(freshness, Ok(true)) {
            let code = if freshness.is_ok() {
                "INPUT_CHANGED"
            } else {
                "FILE_IO_FAILED"
            };
            let owner = Arc::clone(&self.runtime.workspace);
            tokio::task::spawn_blocking(move || {
                let now = now_ms()?;
                owner.workspace_action(session, move |workspace| {
                    workspace.connection.execute(
                        "UPDATE tasks SET status='FAILED',error_code=?4,finished_at_ms=?5,
                            updated_at_ms=?5,row_revision=row_revision+1
                         WHERE task_id=?1 AND capability_id=?2 AND workspace_session_id=?3 AND status='QUEUED'",
                        params![job.task_id.to_string(),CAPABILITY,session.to_string(),code,now],
                    ).map_err(database)?;
                    Ok(())
                })?;
                Ok::<_, WorkerRuntimeError>(())
            }).await.map_err(|_| WorkerRuntimeError::CoreStopped)??;
            return self.runtime.outcome(job.task_id).await;
        }
        let parameters = FrameResolveInput {
            source_video: Some(WorkerInputFile {
                path: source
                    .to_str()
                    .ok_or(WorkerRuntimeError::InvalidBinding)?
                    .into(),
                sha256: format_digest(&loaded.frame.source_digest),
            }),
            input_manifest: Some(WorkerInputFile {
                path: loaded
                    .root
                    .join(&loaded.input_path)
                    .to_str()
                    .ok_or(WorkerRuntimeError::InvalidBinding)?
                    .into(),
                sha256: format_digest(&loaded.frozen.sha256),
            }),
            stream_index: loaded.frame.stream_index,
            frame_index: Some(loaded.frame.frame_index),
            expected_raw_pts: loaded.frame.raw_pts,
            display_width: loaded.frame.width,
            display_height: loaded.frame.height,
            max_pixels: 100_000_000,
            max_frames: 100_000,
            timeout_ms: 30_000,
            ..Default::default()
        };
        self.runtime
            .execute(
                WorkerJob {
                    task: job.task_id,
                    input_fingerprint: loaded.frozen.input_fingerprint,
                    manifest_sha: loaded.frozen.sha256,
                    manifest_size: loaded.frozen.bytes.len() as u64,
                    operation: WorkerOperation::FrameResolve(parameters),
                },
                Duration::from_secs(70),
            )
            .await
    }

    pub async fn shutdown(self) -> Result<(), WorkerRuntimeError> {
        self.runtime.shutdown().await
    }

    pub async fn terminate(self) -> Result<(), WorkerRuntimeError> {
        self.runtime.terminate().await
    }
}

struct StoredFrameTask {
    root: PathBuf,
    created_at_ms: i64,
    session: Uuid,
    status: String,
    error_code: Option<String>,
    input_path: PathBuf,
    input_fingerprint: String,
    attempt: Option<Uuid>,
    output: Option<String>,
    result_reference: Option<String>,
}

struct LoadedFrameTask {
    root: PathBuf,
    created_at_ms: i64,
    session: Uuid,
    status: String,
    error_code: Option<String>,
    input_path: PathBuf,
    frozen: FrozenInputManifest,
    frame: FrameRef,
    attempt: Option<Uuid>,
    output: Option<String>,
    result_reference: Option<String>,
}

fn load_task(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
    contracts: &JsonContracts,
) -> Result<LoadedFrameTask, WorkerRuntimeError> {
    let record = owner.workspace_action(session, move |workspace| {
        workspace.connection.query_row(
            "SELECT workspace_session_id,status,error_code,input_manifest_path,input_fingerprint,current_attempt_id,
                output_manifest_path,json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref'),created_at_ms
             FROM tasks WHERE task_id=?1 AND capability_id=?2 AND operation_kind=?3",
            params![task.to_string(),CAPABILITY,KIND],
            |row| {
                let attempt = row.get::<_, Option<String>>(5)?.map(|value| Uuid::parse_str(&value)
                    .map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?;
                Ok(StoredFrameTask {
                    root: workspace.root().to_owned(),created_at_ms:row.get(8)?,session: row_uuid(row,0)?,status:row.get(1)?,
                    error_code:row.get(2)?,input_path:PathBuf::from(row.get::<_, String>(3)?),
                    input_fingerprint:row.get(4)?,attempt,output:row.get(6)?,result_reference:row.get(7)?,
                })
            },
        ).optional().map_err(database)?
            .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
    })?;
    let mut bytes = Vec::new();
    open_scoped_file(&record.root, &record.input_path)?
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > INPUT_LIMIT {
        return Err(WorkerRuntimeError::InvalidBinding);
    }
    let value = contracts
        .validate(JsonProfile::InputManifest, &bytes)
        .map_err(|error| WorkerRuntimeError::TaskEvidence(Box::new(error)))?;
    let frozen =
        freeze_input_manifest(contracts, value.clone()).map_err(WorkerRuntimeError::Fingerprint)?;
    let config: FrameConfiguration = serde_json::from_value(value["resolved_config"].clone())
        .map_err(|_| WorkerRuntimeError::InvalidBinding)?;
    let assets = value["assets"]
        .as_array()
        .ok_or(WorkerRuntimeError::InvalidBinding)?;
    if assets.len() != 1
        || value["capability_id"] != CAPABILITY
        || assets[0]["asset_id"] != config.video_id.to_string()
        || record.input_fingerprint != format_digest(&frozen.input_fingerprint)
        || frozen.bytes != bytes
    {
        return Err(WorkerRuntimeError::InvalidBinding);
    }
    let digest = assets[0]["media_digest"]
        .as_str()
        .and_then(parse_digest)
        .ok_or(WorkerRuntimeError::InvalidBinding)?;
    let frame = config.frame(digest)?;
    Ok(LoadedFrameTask {
        root: record.root,
        created_at_ms: record.created_at_ms,
        session: record.session,
        status: record.status,
        error_code: record.error_code,
        input_path: record.input_path,
        frozen,
        frame,
        attempt: record.attempt,
        output: record.output,
        result_reference: record.result_reference,
    })
}

fn registered_source(
    owner: &WorkspaceThread,
    session: Uuid,
    video: Uuid,
    digest: [u8; 32],
) -> Result<PathBuf, WorkerRuntimeError> {
    let (root,path,location,expected) = owner.workspace_action(session, move |workspace| {
        workspace.connection.query_row(
            "SELECT path,location_kind,media_digest FROM assets WHERE asset_id=?1 AND kind='VIDEO'",
            [video.to_string()],
            |row| Ok((workspace.root().to_owned(),row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)),
        ).map_err(database)
    })?;
    if parse_digest(&expected) != Some(digest) {
        return Err(WorkerRuntimeError::InvalidBinding);
    }
    let path = PathBuf::from(path);
    match location.as_str() {
        "EXTERNAL" if path.is_absolute() => Ok(path),
        "WORKSPACE" if is_portable_relative_file(&path) => Ok(root.join(path)),
        _ => Err(WorkerRuntimeError::InvalidBinding),
    }
}

fn signed_decimal(text: &str) -> Result<i64, WorkerRuntimeError> {
    let value = text
        .parse::<i64>()
        .map_err(|_| WorkerRuntimeError::InvalidBinding)?;
    if value.to_string() != text {
        return Err(WorkerRuntimeError::InvalidBinding);
    }
    Ok(value)
}

fn request_digest(request: FrameSubmission) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"datasetdoctor.task.frame-resolve.v1\0");
    hash.update(request.video_id.as_bytes());
    let (kind, value) = match request.target {
        FrameTarget::Index(value) => (0_u8, value),
        FrameTarget::TimestampMs(value) => (1_u8, value),
    };
    hash.update([kind]);
    hash.update(value.to_le_bytes());
    hash.update([match request.policy {
        SeekPolicy::Nearest => 0,
        SeekPolicy::AtOrAfter => 1,
    }]);
    hash.finalize().into()
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
