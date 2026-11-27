//! Queue and execute a frozen Dataset sampling request through real frame workers.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    ResolvedVideoSampling, StagedVideoSample, VideoAnnotationError, VideoAnnotationService,
    VideoSamplingMode, VideoSamplingPurpose, VideoSamplingRequest, database, digest,
    invalid_storage,
};
use crate::frame_runtime::{
    FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget, SeekPolicy,
};
use crate::input_fingerprint::{FrozenInputManifest, freeze_input_manifest};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, format_digest};
use crate::video_annotation_store::VideoArtifactReference;
use crate::video_query::{VideoSourceAvailability, registered_video_detail};
use crate::worker_process::WorkerProgram;
use crate::worker_runtime::now_ms;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const KIND: &str = "VIDEO_SAMPLE";
const CAPABILITY: &str = "video_dataset_sampling.v1";
const MAX_RECEIPT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct VideoSampleSubmission {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub selection: VideoSamplingRequest,
}

pub struct VideoSampleJob {
    task_id: Uuid,
    plan: ResolvedVideoSampling,
    input: FrozenInputManifest,
    tracking_ref: Option<VideoArtifactReference>,
}

impl VideoSampleJob {
    pub fn task_id(&self) -> Uuid {
        self.task_id
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoSampleReceipt {
    pub source_video_id: Uuid,
    pub target_dataset_id: Uuid,
    pub segment_id: Uuid,
    pub segment_revision: u64,
    pub sampling_config_id: Uuid,
    pub samples: Vec<StagedVideoSample>,
}

#[derive(Debug)]
pub struct VideoSampleSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub accepted_at_ms: i64,
    pub receipt: Option<VideoSampleReceipt>,
}

pub struct VideoSampleService {
    annotation: VideoAnnotationService,
}

impl VideoSampleService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, VideoAnnotationError> {
        Ok(Self {
            annotation: VideoAnnotationService::new(owner)?,
        })
    }

    pub fn submit(
        &self,
        session: Uuid,
        submission: VideoSampleSubmission,
        now: i64,
    ) -> Result<(VideoSampleSnapshot, Option<VideoSampleJob>), VideoAnnotationError> {
        self.submit_scoped(session, submission, None, now)
    }

    pub fn submit_with_tracking(
        &self,
        session: Uuid,
        submission: VideoSampleSubmission,
        tracking: VideoArtifactReference,
        now: i64,
    ) -> Result<(VideoSampleSnapshot, Option<VideoSampleJob>), VideoAnnotationError> {
        self.submit_scoped(session, submission, Some(tracking), now)
    }

    fn submit_scoped(
        &self,
        session: Uuid,
        submission: VideoSampleSubmission,
        tracking_ref: Option<VideoArtifactReference>,
        now: i64,
    ) -> Result<(VideoSampleSnapshot, Option<VideoSampleJob>), VideoAnnotationError> {
        if submission.request_id.is_nil() || submission.idempotency_key.is_nil() {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let owner = &self.annotation.owner;
        let _guard = owner.lock_final_operations()?;
        let mut selection = selection_config(submission.selection)?;
        if let Some(reference) = &tracking_ref {
            selection["tracking_result_ref"] = json!(reference);
        }
        let request_fingerprint: [u8; 32] = Sha256::digest(selection.to_string().as_bytes()).into();
        let key = submission.idempotency_key.to_string();
        let existing = owner.workspace_action(session, move |workspace| {
            workspace.connection.query_row(
                "SELECT task_id,request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
                params![KIND,key], |row| Ok((row_uuid(row,0)?,row.get::<_,String>(1)?)),
            ).optional().map_err(database)
        })?;
        if let Some((task, fingerprint)) = existing {
            if fingerprint != format_digest(&request_fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            let snapshot = self
                .snapshot(session, task, now)?
                .ok_or(VideoAnnotationError::InvalidInput)?;
            return Ok((snapshot, None));
        }
        let plan = self
            .annotation
            .resolve_sampling_locked(session, submission.selection)?;
        if let Some(reference) = &tracking_ref {
            super::tracking_samples::candidate_frames(owner, session, &plan, reference)?;
        }
        let input = self.freeze(&plan, selection)?;
        let root = owner.storage_root(session)?;
        let path = PathBuf::from(input_path(&input));
        publish_immutable(&root, &path, &input.bytes)?;
        let task = owner.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: Some(plan.request.target_dataset_id),
                request_id: submission.request_id,
                idempotency_key: submission.idempotency_key,
                operation_kind: KIND.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: path,
                request_fingerprint,
                input_fingerprint: input.input_fingerprint,
            },
            now,
        )?;
        Ok((
            VideoSampleSnapshot {
                task_id: task.task_id,
                status: task.status,
                accepted_at_ms: task.created_at_ms,
                error_code: None,
                receipt: None,
            },
            Some(VideoSampleJob {
                task_id: task.task_id,
                plan,
                input,
                tracking_ref,
            }),
        ))
    }

    pub async fn run(
        &self,
        session: Uuid,
        job: VideoSampleJob,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<(), VideoAnnotationError> {
        self.run_with_program(
            session,
            job,
            WorkerProgram::Script {
                interpreter,
                entrypoint,
            },
        )
        .await
    }

    pub async fn run_with_program(
        &self,
        session: Uuid,
        job: VideoSampleJob,
        program: WorkerProgram,
    ) -> Result<(), VideoAnnotationError> {
        self.start(session, &job)?;
        let task = job.task_id;
        if self.verify_input(session, &job.input).is_err() {
            return self.fail(session, task, "INPUT_MANIFEST_INVALID");
        }
        let frame_tasks = match self.decode(session, &job, program).await {
            Ok(tasks) => tasks,
            Err(code) => return self.fail(session, task, code),
        };
        let owner = Arc::clone(&self.annotation.owner);
        let sampled = tokio::task::spawn_blocking(move || {
            let service = VideoAnnotationService::new(owner)?;
            let finish = time_now()?;
            let request = job.plan.request;
            let revision = job.plan.segment_revision;
            let config = job.plan.sampling_config_id;
            service.stage_dataset_samples_with(session, &job.plan, &frame_tasks, job.tracking_ref.as_ref(), finish, move |tx, root, items| {
                let payload = json!({
                    "source_video_id":request.source_video_id, "target_dataset_id":request.target_dataset_id,
                    "segment_id":request.segment_id, "segment_revision":revision,
                    "sampling_config_id":config, "samples":items,
                });
                let receipt = serde_json::to_vec(&payload).map_err(|_| invalid_storage())?;
                if receipt.len() as u64 > MAX_RECEIPT_BYTES {
                    return Err(invalid_storage());
                }
                let path = receipt_path(task);
                publish_immutable(root, Path::new(&path), &receipt).map_err(|_| invalid_storage())?;
                let metadata = json!({"terminal_receipt":{"result_manifest_ref":{
                    "sha256":digest(&receipt), "size_bytes":receipt.len().to_string()
                }}}).to_string();
                let changed = tx.execute(
                    "UPDATE tasks SET status='SUCCEEDED', finished_at_ms=?4, updated_at_ms=?4,
                     row_revision=row_revision+1,output_manifest_path=?5,execution_metadata_json=?6
                     WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='RUNNING'",
                    params![task.to_string(),KIND,session.to_string(),finish,path,metadata],
                ).map_err(database)?;
                if changed != 1 {
                    return Err(invalid_storage());
                }
                Ok(())
            })
        }).await.map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
        match sampled {
            Ok(_) => Ok(()),
            Err(VideoAnnotationError::RevisionConflict) => {
                self.fail(session, task, "INPUT_CHANGED")
            }
            Err(VideoAnnotationError::TrackingEvidenceInvalid) => {
                self.fail(session, task, "TRACKING_EVIDENCE_INVALID")
            }
            Err(_) => self.fail(session, task, "STAGING_PUBLICATION_FAILED"),
        }
    }

    pub fn record_worker_unavailable(
        &self,
        session: Uuid,
        job: VideoSampleJob,
    ) -> Result<(), VideoAnnotationError> {
        self.start(session, &job)?;
        self.fail(session, job.task_id, "WORKER_START_FAILED")
    }

    pub fn snapshot(
        &self,
        session: Uuid,
        task: Uuid,
        now: i64,
    ) -> Result<Option<VideoSampleSnapshot>, VideoAnnotationError> {
        let loaded = self.annotation.owner.workspace_action(session, move |workspace| {
            let task_text = task.to_string();
            workspace.connection.execute(
                "UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',finished_at_ms=?4,
                    updated_at_ms=?4,row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3 AND status IN ('QUEUED','RUNNING')",
                params![task_text,KIND,session.to_string(),now],
            ).map_err(database)?;
            workspace.connection.query_row(
                "SELECT status,error_code,created_at_ms,output_manifest_path,
                    json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref.sha256')
                 FROM tasks WHERE task_id=?1 AND operation_kind=?2",
                params![task_text,KIND],
                |row| Ok((VideoSampleSnapshot {
                    task_id:task,status:row.get(0)?,error_code:row.get(1)?,
                    accepted_at_ms:row.get(2)?,receipt:None
                },row.get::<_,Option<String>>(3)?,row.get::<_,Option<String>>(4)?,workspace.root().to_owned())),
            ).optional().map_err(database)
        })?;
        let Some((mut snapshot, path, sha256, root)) = loaded else {
            return Ok(None);
        };
        if snapshot.status == "SUCCEEDED" {
            let path = path
                .filter(|path| *path == receipt_path(task))
                .ok_or(VideoAnnotationError::InvalidInput)?;
            let mut bytes = Vec::new();
            open_scoped_file(&root, Path::new(&path))?
                .take(MAX_RECEIPT_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_RECEIPT_BYTES
                || sha256.as_deref() != Some(digest(&bytes).as_str())
            {
                return Err(VideoAnnotationError::InvalidInput);
            }
            snapshot.receipt = Some(
                serde_json::from_slice(&bytes).map_err(|_| VideoAnnotationError::InvalidInput)?,
            );
        }
        Ok(Some(snapshot))
    }

    fn freeze(
        &self,
        plan: &ResolvedVideoSampling,
        selection: Value,
    ) -> Result<FrozenInputManifest, VideoAnnotationError> {
        let frames: Vec<Value> = plan.frames.iter().map(|frame| json!({
            "frame_index":frame.frame_index.to_string(),"stream_index":frame.stream_index,
            "pts_us":frame.pts_us.map(|value|value.to_string()),"raw_pts":frame.raw_pts.map(|value|value.to_string()),
            "width":frame.width,"height":frame.height,
        })).collect();
        let dd_domain::video::FrameRange::Time { start_us, end_us } = plan.range else {
            return Err(VideoAnnotationError::InvalidInput);
        };
        let clock = plan
            .source
            .time_base
            .ok_or(VideoAnnotationError::InvalidInput)?;
        let value = json!({
            "capability_id":CAPABILITY,"implementation_version":"1","model_identity":null,
            "dataset_id":plan.request.target_dataset_id,"label_schema_digest":plan.label_schema_digest,
            "assets":[{"asset_id":plan.request.source_video_id,
                "media_digest":format_digest(&plan.source.source_digest),
                "annotation_digest":null,"source_metadata_digest":plan.video_annotation_manifest_ref.sha256}],
            "resolved_config":{
                "selection":selection,"sampling_config_id":plan.sampling_config_id,
                "segment_revision":plan.segment_revision.to_string(),
                "start_us":start_us.to_string(),"end_us":end_us.to_string(),
                "time_base":[clock.numerator,clock.denominator],
                "video_annotation_manifest_ref":plan.video_annotation_manifest_ref,
                "frames":frames,"keyframes":plan.keyframes,
            }
        });
        freeze_input_manifest(&self.annotation.contracts, value)
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))
    }

    fn start(&self, session: Uuid, job: &VideoSampleJob) -> Result<(), VideoAnnotationError> {
        let owner = &self.annotation.owner;
        let task = job.task_id.to_string();
        let proof = format_digest(&job.input.input_fingerprint);
        let expected_path = input_path(&job.input);
        let now = time_now()?;
        let changed = owner.workspace_action(session, move |workspace| {
            workspace.connection.execute(
                "UPDATE tasks SET status='RUNNING',started_at_ms=?4,updated_at_ms=?4,row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'
                 AND input_fingerprint=?5 AND input_manifest_path=?6",
                params![task,KIND,session.to_string(),now,proof,expected_path],
            ).map_err(database)
        })?;
        if changed != 1 {
            return Err(VideoAnnotationError::InvalidInput);
        }
        Ok(())
    }

    fn verify_input(
        &self,
        session: Uuid,
        input: &FrozenInputManifest,
    ) -> Result<(), VideoAnnotationError> {
        let root = self.annotation.owner.storage_root(session)?;
        let mut bytes = Vec::new();
        open_scoped_file(&root, Path::new(&input_path(input)))?
            .take(input.bytes.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != input.bytes {
            return Err(VideoAnnotationError::InvalidInput);
        }
        Ok(())
    }

    async fn decode(
        &self,
        session: Uuid,
        job: &VideoSampleJob,
        program: WorkerProgram,
    ) -> Result<Vec<Uuid>, &'static str> {
        let owner = &self.annotation.owner;
        let video = job.plan.request.source_video_id;
        let detail =
            registered_video_detail(owner, session, video).map_err(|_| "FRAME_EVIDENCE_INVALID")?;
        if detail.source_availability != VideoSourceAvailability::Online
            || detail.source_digest != format_digest(&job.plan.source.source_digest)
        {
            return Err("INPUT_CHANGED");
        }
        let dataset = owner
            .dataset_detail(session, job.plan.request.target_dataset_id)
            .map_err(|_| "INPUT_CHANGED")?;
        if dataset.summary.schema_fingerprint != job.plan.label_schema_digest {
            return Err("INPUT_CHANGED");
        }
        let frames = FrameService::new(Arc::clone(owner)).map_err(|_| "FRAME_EVIDENCE_INVALID")?;
        let mut worker: Option<FrameResolveRuntime> = None;
        let mut tasks = Vec::with_capacity(job.plan.frames.len());
        let result = async {
            for expected in &job.plan.frames {
                let (snapshot, pending) = frames
                    .submit(
                        session,
                        FrameSubmission {
                            request_id: Uuid::new_v4(),
                            idempotency_key: Uuid::new_v4(),
                            video_id: video,
                            target: FrameTarget::Index(expected.frame_index),
                            policy: SeekPolicy::AtOrAfter,
                        },
                        time_now().map_err(|_| "INTERNAL")?,
                    )
                    .map_err(|_| "FRAME_EVIDENCE_INVALID")?;
                if let Some(pending) = pending {
                    if worker.is_none() {
                        match FrameResolveRuntime::launch_with_program(
                            Arc::clone(owner),
                            session,
                            program.clone(),
                        )
                        .await
                        {
                            Ok(runtime) => worker = Some(runtime),
                            Err(_) => {
                                let _ = FrameResolveRuntime::record_unavailable(
                                    owner,
                                    session,
                                    snapshot.task_id,
                                    true,
                                );
                                return Err("WORKER_START_FAILED");
                            }
                        }
                    }
                    let outcome = match worker
                        .as_mut()
                        .ok_or("WORKER_START_FAILED")?
                        .run(pending)
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(_) => {
                            let runtime = worker.take().ok_or("WORKER_INTERRUPTED")?;
                            if runtime.terminate().await.is_ok() {
                                let _ = FrameResolveRuntime::record_unavailable(
                                    owner,
                                    session,
                                    snapshot.task_id,
                                    false,
                                );
                            }
                            return Err("WORKER_INTERRUPTED");
                        }
                    };
                    if outcome.status != "SUCCEEDED" {
                        return Err(match outcome.error_code.as_deref() {
                            Some("INPUT_CHANGED") => "INPUT_CHANGED",
                            Some("FILE_IO_FAILED") => "FILE_IO_FAILED",
                            _ => "FRAME_DECODE_FAILED",
                        });
                    }
                } else if snapshot.status != "SUCCEEDED" {
                    return Err("FRAME_EVIDENCE_INVALID");
                }
                tasks.push(snapshot.task_id);
            }
            Ok(tasks)
        }
        .await;
        if let Some(worker) = worker {
            let _ = worker.shutdown().await;
        }
        result
    }

    fn fail(
        &self,
        session: Uuid,
        task: Uuid,
        error: &'static str,
    ) -> Result<(), VideoAnnotationError> {
        let now = time_now()?;
        let updated = self.annotation.owner.workspace_action(session, move |workspace| {
            workspace.connection.execute(
                "UPDATE tasks SET status='FAILED',error_code=?4,finished_at_ms=?5,updated_at_ms=?5,row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='RUNNING'",
                params![task.to_string(),KIND,session.to_string(),error,now],
            ).map_err(database)
        })?;
        if updated != 1 {
            return Err(VideoAnnotationError::InvalidInput);
        }
        Ok(())
    }
}

fn selection_config(request: VideoSamplingRequest) -> Result<Value, VideoAnnotationError> {
    let VideoSamplingPurpose::Dataset {
        include_keyframes,
        max_frames,
    } = request.purpose
    else {
        return Err(VideoAnnotationError::InvalidInput);
    };
    let sampling = match request.mode {
        VideoSamplingMode::EveryNFrames { every_n } => {
            json!({"mode":"EVERY_N_FRAMES","every_n":every_n})
        }
        VideoSamplingMode::TimeInterval { interval_ms } => {
            json!({"mode":"TIME_INTERVAL","interval_ms":interval_ms})
        }
    };
    Ok(json!({
        "source_video_id":request.source_video_id,"segment_id":request.segment_id,
        "target_dataset_id":request.target_dataset_id,"sampling":sampling,
        "include_keyframes":include_keyframes,"max_frames":max_frames,
    }))
}

fn receipt_path(task: Uuid) -> String {
    format!("artifacts/tasks/{task}/sample-receipt.json")
}

fn input_path(manifest: &FrozenInputManifest) -> String {
    format!(
        "artifacts/task-inputs/{}.json",
        format_digest(&manifest.sha256)
    )
}

fn time_now() -> Result<i64, VideoAnnotationError> {
    now_ms().map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))
}
