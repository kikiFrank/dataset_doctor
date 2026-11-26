//! Freeze Workspace tracking inputs and run the dedicated Worker attempt.

mod binding;
mod result;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use crate::worker_runtime::WorkerRuntimeError as TrackingError;
use binding::{TrackingBinding, freeze_plan};
pub use result::{
    TrackObservation, TrackTailCheck, TrackUncertainRange, VideoTrackBundle, VideoTrackData,
};

use crate::frame_runtime::registered_source;
use crate::input_fingerprint::{FrozenInputManifest, freeze_input_manifest};
use crate::json::{JsonContracts, JsonProfile};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskOutcome, TaskStoreError, TaskSubmission, format_digest, parse_digest};
use crate::video_keyframes::{
    VideoAnnotationService, VideoSamplingMode, VideoSamplingPurpose, VideoSamplingRequest,
};
use crate::video_query::{VideoSourceAvailability, registered_video_detail};
use crate::worker_runtime::{WorkerJob, WorkerOperation, WorkerRuntime, now_ms};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub(crate) const CAPABILITY: &str = "video_tracking.v1";
const KIND: &str = "VIDEO_TRACK";
const INPUT_LIMIT: u64 = 2 * 1024 * 1024;
pub const TRANSLATION_PROFILE_ID: Uuid = Uuid::from_u128(0x95210a17_0fd9_577d_9cb2_82f827fa5e0b);

#[derive(Clone, Copy)]
pub struct VideoTrackingSubmission {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub tracker_profile_id: Uuid,
    pub selection: VideoSamplingRequest,
}

pub struct VideoTrackingJob {
    task_id: Uuid,
}

impl VideoTrackingJob {
    pub fn task_id(&self) -> Uuid {
        self.task_id
    }
}

#[derive(Debug)]
pub struct VideoTrackingSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub accepted_at_ms: i64,
    pub bundle: Option<VideoTrackBundle>,
}

pub struct VideoTrackingService {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

impl VideoTrackingService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, TrackingError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(TrackingError::TaskEvidence)?,
        })
    }

    pub fn submit(
        &self,
        session: Uuid,
        request: VideoTrackingSubmission,
        now: i64,
    ) -> Result<(VideoTrackingSnapshot, Option<VideoTrackingJob>), TrackingError> {
        let VideoSamplingPurpose::Tracking {
            supports_time_interval,
        } = request.selection.purpose
        else {
            return Err(TrackingError::InvalidBinding);
        };
        if request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || now < 0
            || request.tracker_profile_id != TRANSLATION_PROFILE_ID
        {
            return Err(TrackingError::InvalidBinding);
        }
        let selection = request_config(request, supports_time_interval);
        let intent: [u8; 32] = Sha256::digest(selection.to_string().as_bytes()).into();
        let key = request.idempotency_key.to_string();
        let _guard = self.owner.lock_final_operations()?;
        let existing = self.owner.workspace_action(session, move |workspace| {
            workspace.connection.query_row(
                "SELECT task_id,request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
                params![KIND,key], |row| Ok((row_uuid(row,0)?,row.get::<_,String>(1)?)),
            ).optional().map_err(database)
        })?;
        if let Some((task, original)) = existing {
            if original != format_digest(&intent) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return Ok((self.snapshot(session, task)?, None));
        }
        let work = VideoAnnotationService::new(Arc::clone(&self.owner))
            .map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
        let plan = work
            .resolve_sampling_locked(session, request.selection)
            .map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
        let frozen = freeze_plan(&self.contracts, &work, session, &plan, selection)?;
        let root = self.owner.storage_root(session)?;
        let path = manifest_path(&frozen);
        publish_immutable(&root, &path, &frozen.bytes)?;
        let task = self.owner.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: Some(request.selection.target_dataset_id),
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: KIND.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: path,
                request_fingerprint: intent,
                input_fingerprint: frozen.input_fingerprint,
            },
            now,
        )?;
        Ok((
            VideoTrackingSnapshot {
                task_id: task.task_id,
                status: task.status,
                error_code: None,
                accepted_at_ms: task.created_at_ms,
                bundle: None,
            },
            Some(VideoTrackingJob {
                task_id: task.task_id,
            }),
        ))
    }

    pub fn snapshot(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<VideoTrackingSnapshot, TrackingError> {
        let record = read_task(&self.owner, session, task)?;
        let mut snapshot = VideoTrackingSnapshot {
            task_id: task,
            status: record.status.clone(),
            error_code: record.error_code.clone(),
            accepted_at_ms: record.created_at_ms,
            bundle: None,
        };
        if record.status == "SUCCEEDED" {
            let loaded = load_binding(record, &self.contracts)?;
            snapshot.bundle = Some(result::read_bundle(task, &loaded, &self.contracts)?);
        }
        Ok(snapshot)
    }
}

pub struct VideoTrackingRuntime {
    runtime: WorkerRuntime,
    contracts: Arc<JsonContracts>,
}

impl VideoTrackingRuntime {
    pub async fn launch(
        owner: Arc<WorkspaceThread>,
        session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, TrackingError> {
        Ok(Self {
            runtime: WorkerRuntime::launch(owner, session, interpreter, entrypoint, CAPABILITY)
                .await?,
            contracts: Arc::new(JsonContracts::new().map_err(TrackingError::TaskEvidence)?),
        })
    }

    pub async fn run(&mut self, job: VideoTrackingJob) -> Result<TaskOutcome, TrackingError> {
        let owner = Arc::clone(&self.runtime.workspace);
        let session = self.runtime.session.workspace_session_id;
        let contracts = Arc::clone(&self.contracts);
        let task = job.task_id;
        let prepared =
            tokio::task::spawn_blocking(move || prepare(&owner, session, task, &contracts))
                .await
                .map_err(|_| TrackingError::CoreStopped)??;
        match prepared {
            Some(job) => self.runtime.execute(job, Duration::from_secs(180)).await,
            None => self.runtime.outcome(task).await,
        }
    }

    pub async fn shutdown(self) -> Result<(), TrackingError> {
        self.runtime.shutdown().await
    }
}

fn prepare(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
    contracts: &JsonContracts,
) -> Result<Option<WorkerJob>, TrackingError> {
    let record = read_task(owner, session, task)?;
    if record.status != "QUEUED" {
        return Ok(None);
    }
    let Ok(loaded) = load_binding(record, contracts) else {
        fail_queued(owner, session, task, "INPUT_MANIFEST_INVALID")?;
        return Ok(None);
    };
    let source = registered_video_detail(owner, session, loaded.video);
    let current = owner.dataset_detail(session, loaded.record.dataset);
    if !source.as_ref().is_ok_and(|video| {
        video.source_availability == VideoSourceAvailability::Online
            && video.source_digest == format_digest(&loaded.digest)
    }) || !current.is_ok_and(|dataset| dataset.summary.schema_fingerprint == loaded.schema)
    {
        fail_queued(owner, session, task, "INPUT_CHANGED")?;
        return Ok(None);
    }
    let source_path = registered_source(owner, session, loaded.video, loaded.digest)?;
    let parameters = loaded.binding.parameters(
        &source_path,
        &loaded.digest,
        &loaded.record.root.join(&loaded.record.input_path),
        &loaded.frozen.sha256,
    )?;
    Ok(Some(WorkerJob {
        task,
        input_fingerprint: loaded.frozen.input_fingerprint,
        manifest_sha: loaded.frozen.sha256,
        manifest_size: loaded.frozen.bytes.len() as u64,
        operation: WorkerOperation::VideoTracking(parameters),
    }))
}

struct StoredTrackingTask {
    root: PathBuf,
    dataset: Uuid,
    created_at_ms: i64,
    status: String,
    error_code: Option<String>,
    input_path: PathBuf,
    input_fingerprint: String,
    attempt: Option<Uuid>,
    output: Option<PathBuf>,
    result_reference: Option<String>,
}

struct LoadedTrackingTask {
    record: StoredTrackingTask,
    binding: TrackingBinding,
    frozen: FrozenInputManifest,
    video: Uuid,
    digest: [u8; 32],
    schema: String,
}

fn read_task(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
) -> Result<StoredTrackingTask, TrackingError> {
    let now = now_ms()?;
    Ok(owner.workspace_action(session, move |workspace| {
        workspace.connection.execute(
            "UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',finished_at_ms=?4,
                updated_at_ms=?4,row_revision=row_revision+1,
                execution_metadata_json=json_set(execution_metadata_json,'$.grant_state','REVOKED')
             WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3
               AND status IN ('QUEUED','DISPATCHING','RUNNING','CANCELLING')",
            params![task.to_string(), KIND, session.to_string(), now],
        ).map_err(database)?;
        workspace.connection.query_row(
            "SELECT dataset_id,created_at_ms,status,error_code,input_manifest_path,input_fingerprint,current_attempt_id,
                    output_manifest_path,json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
             FROM tasks WHERE task_id=?1 AND operation_kind=?2 AND capability_id=?3",
            params![task.to_string(),KIND,CAPABILITY],
            |row| Ok(StoredTrackingTask {
                root: workspace.root().to_owned(), dataset:row_uuid(row,0)?, created_at_ms:row.get(1)?,
                status:row.get(2)?, error_code:row.get(3)?, input_path:PathBuf::from(row.get::<_,String>(4)?),
                input_fingerprint:row.get(5)?,
                attempt:row.get::<_,Option<String>>(6)?.map(|id|Uuid::parse_str(&id)
                    .map_err(|_|rusqlite::Error::InvalidQuery)).transpose()?,
                output:row.get::<_,Option<String>>(7)?.map(PathBuf::from), result_reference:row.get(8)?,
            }),
        ).optional().map_err(database)?.ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
    })?)
}

fn load_binding(
    record: StoredTrackingTask,
    contracts: &JsonContracts,
) -> Result<LoadedTrackingTask, TrackingError> {
    let invalid = || TrackingError::InvalidBinding;
    let mut bytes = Vec::new();
    open_scoped_file(&record.root, &record.input_path)?
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > INPUT_LIMIT {
        return Err(invalid());
    }
    let value = contracts
        .validate(JsonProfile::InputManifest, &bytes)
        .map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
    let frozen =
        freeze_input_manifest(contracts, value.clone()).map_err(TrackingError::Fingerprint)?;
    if record.input_path != manifest_path(&frozen)
        || bytes != frozen.bytes
        || record.input_fingerprint != format_digest(&frozen.input_fingerprint)
        || value["capability_id"] != CAPABILITY
        || value["dataset_id"] != record.dataset.to_string()
    {
        return Err(invalid());
    }
    let [source] = value["assets"].as_array().ok_or_else(invalid)?.as_slice() else {
        return Err(invalid());
    };
    let video = source["asset_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(invalid)?;
    let digest = source["media_digest"]
        .as_str()
        .and_then(parse_digest)
        .ok_or_else(invalid)?;
    let schema = value["label_schema_digest"]
        .as_str()
        .ok_or_else(invalid)?
        .to_owned();
    let binding: TrackingBinding =
        serde_json::from_value(value["resolved_config"]["tracking_binding"].clone())
            .map_err(|_| invalid())?;
    if binding.tracker_profile_id != TRANSLATION_PROFILE_ID
        || binding.seeds.is_empty()
        || binding.frames.is_empty()
    {
        return Err(invalid());
    }
    Ok(LoadedTrackingTask {
        record,
        binding,
        frozen,
        video,
        digest,
        schema,
    })
}

fn request_config(request: VideoTrackingSubmission, supports_time_interval: bool) -> Value {
    let selection = match request.selection.mode {
        VideoSamplingMode::EveryNFrames { every_n } => {
            json!({"mode":"EVERY_N_FRAMES","every_n":every_n})
        }
        VideoSamplingMode::TimeInterval { interval_ms } => {
            json!({"mode":"TIME_INTERVAL","interval_ms":interval_ms})
        }
    };
    json!({
        "source_video_id":request.selection.source_video_id, "segment_id":request.selection.segment_id,
        "target_dataset_id":request.selection.target_dataset_id, "profile_id":request.tracker_profile_id,
        "sampling":selection, "supports_time_interval":supports_time_interval,
    })
}

fn manifest_path(frozen: &FrozenInputManifest) -> PathBuf {
    Path::new("artifacts/task-inputs").join(format!("{}.json", format_digest(&frozen.sha256)))
}

fn fail_queued(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
    code: &'static str,
) -> Result<(), TrackingError> {
    let now = now_ms()?;
    Ok(owner.workspace_action(session, move |workspace| {
        workspace.connection.execute(
            "UPDATE tasks SET status='FAILED',error_code=?4,finished_at_ms=?5,updated_at_ms=?5,row_revision=row_revision+1
             WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'",
            params![task.to_string(),KIND,session.to_string(),code,now],
        ).map_err(database)?;
        Ok(())
    })?)
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
