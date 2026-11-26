//! Register verified frame results as managed images awaiting checks and review.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dd_domain::video::{FrameRef, TimeBase};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ResolvedVideoSampling, VideoAnnotationError, VideoAnnotationService, VideoSamplingMode,
    VideoSamplingPurpose, database, digest, invalid_storage,
};
use crate::frame_runtime::FrameService;
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::format_digest;
use crate::video_annotation_store::{VideoFrameReference, read_keyframe};
use crate::video_index::ResolvedVideoFrame;
use crate::video_query::{VideoSourceAvailability, registered_video_detail};
use crate::workspace_thread::WorkspaceThreadError;

const MAX_FRAME_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StagedVideoSample {
    pub asset_id: Uuid,
    pub frame_index: u64,
    pub already_in_dataset: bool,
}

struct PreparedSample {
    frame: FrameRef,
    image_relative: PathBuf,
    image_sha256: String,
    image_size: u64,
    target_relative: String,
    filename: String,
    annotation: Option<PreparedAnnotation>,
    source_metadata: String,
}

struct PreparedAnnotation {
    bytes: Vec<u8>,
    local_image_id: i64,
    relative_path: String,
    sha256: String,
}

impl VideoAnnotationService {
    pub fn stage_dataset_samples(
        &self,
        session: Uuid,
        plan: &ResolvedVideoSampling,
        frame_tasks: &[Uuid],
        now_ms: i64,
    ) -> Result<Vec<StagedVideoSample>, VideoAnnotationError> {
        self.stage_dataset_samples_with(session, plan, frame_tasks, now_ms, |_, _, _| Ok(()))
    }

    pub(super) fn stage_dataset_samples_with(
        &self,
        session: Uuid,
        plan: &ResolvedVideoSampling,
        frame_tasks: &[Uuid],
        now_ms: i64,
        finish: impl FnOnce(
            &Transaction<'_>,
            &Path,
            &[StagedVideoSample],
        ) -> Result<(), WorkspaceThreadError>
        + Send
        + 'static,
    ) -> Result<Vec<StagedVideoSample>, VideoAnnotationError> {
        let VideoSamplingPurpose::Dataset {
            include_keyframes,
            max_frames,
        } = plan.request.purpose
        else {
            return Err(VideoAnnotationError::InvalidInput);
        };
        if plan.frames.is_empty() || plan.frames.len() != frame_tasks.len() {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let time_base = plan
            .source
            .time_base
            .ok_or(VideoAnnotationError::InvalidInput)?;
        let _guard = self.owner.lock_final_operations()?;
        let video = plan.request.source_video_id;
        let dataset = plan.request.target_dataset_id;
        let binding = self.owner.dataset_detail(session, dataset)?;
        if binding.summary.schema_fingerprint != plan.label_schema_digest {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        let snapshot = self.snapshot(session, video)?;
        let source_digest = format_digest(&plan.source.source_digest);
        let detail = registered_video_detail(&self.owner, session, video)
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
        if snapshot.digest != source_digest
            || detail.source_availability != VideoSourceAvailability::Online
            || detail.source_digest != source_digest
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let mut config = match plan.request.mode {
            VideoSamplingMode::EveryNFrames { every_n } => {
                json!({"mode":"EVERY_N_FRAMES","every_n":every_n})
            }
            VideoSamplingMode::TimeInterval { interval_ms } => {
                json!({"mode":"TIME_INTERVAL","interval_ms":interval_ms})
            }
        };
        config["sampling_config_id"] = json!(plan.sampling_config_id);
        config["include_keyframes"] = json!(include_keyframes);
        if let Some(limit) = max_frames {
            config["max_frames"] = json!(limit);
        }
        let frames = FrameService::new(Arc::clone(&self.owner))
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
        let mut prepared = Vec::with_capacity(frame_tasks.len());
        for (&frame, &task) in plan.frames.iter().zip(frame_tasks) {
            let result = frames
                .snapshot(session, task)
                .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
            let attempt = result
                .attempt_id
                .ok_or(VideoAnnotationError::InvalidInput)?;
            let resolved = result.frame.ok_or(VideoAnnotationError::InvalidInput)?;
            if resolved.frame != frame
                || frame.source_video_id != video
                || frame.source_digest != plan.source.source_digest
                || frame.stream_index != plan.source.stream_index
            {
                return Err(VideoAnnotationError::InvalidInput);
            }
            let sample = PreparedSample::new(
                &snapshot.root,
                resolved,
                plan,
                task,
                attempt,
                time_base,
                &config,
            )?;
            prepared.push(sample);
        }
        for sample in &prepared {
            sample.publish(&snapshot.root)?;
        }
        // Recheck the source once after all blob writes, before registering any assets.
        let current = registered_video_detail(&self.owner, session, video)
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
        if current.source_availability != VideoSourceAvailability::Online
            || current.source_digest != source_digest
        {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        let schema_digest = plan.label_schema_digest.clone();
        self.owner
            .workspace_action(session, move |workspace| {
                let root = workspace.root().to_owned();
                let tx = workspace.connection.transaction().map_err(database)?;
                let categories: String = tx
                    .query_row(
                        "SELECT categories_json FROM datasets WHERE dataset_id=?1",
                        [dataset.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(database)?;
                if digest(categories.as_bytes()) != schema_digest {
                    return Err(invalid_storage());
                }
                let valid_source: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM assets WHERE asset_id=?1
                     AND kind='VIDEO' AND media_digest=?2 AND availability='ONLINE')",
                        params![video.to_string(), source_digest],
                        |row| row.get(0),
                    )
                    .map_err(database)?;
                if !valid_source {
                    return Err(invalid_storage());
                }
                let staged = prepared
                    .iter()
                    .map(|sample| sample.register(&tx, dataset, video, now_ms))
                    .collect::<Result<Vec<_>, _>>()?;
                finish(&tx, &root, &staged)?;
                tx.commit().map_err(database)?;
                Ok(staged)
            })
            .map_err(VideoAnnotationError::Storage)
    }
}

impl PreparedSample {
    fn new(
        root: &Path,
        resolved: ResolvedVideoFrame,
        plan: &ResolvedVideoSampling,
        task: Uuid,
        attempt: Uuid,
        clock: TimeBase,
        config: &Value,
    ) -> Result<Self, VideoAnnotationError> {
        let frame = resolved.frame;
        let reference = VideoFrameReference::from_resolved(task, &resolved)?;
        let filename = format!("frame-{}.png", frame.frame_index);
        let prefix = format!(
            "videos/{}/{}/stream-{}",
            frame.source_video_id, reference.source_digest, frame.stream_index
        );
        let annotation = plan
            .keyframes
            .iter()
            .find(|keyframe| keyframe.frame_ref.frame_index == reference.frame_index)
            .map(|keyframe| {
                let draft = read_keyframe(root, frame.source_video_id, &keyframe.annotation_ref)?;
                if draft.frame_ref != keyframe.frame_ref
                    || keyframe.frame_ref != reference
                    || draft.target_dataset_id != plan.request.target_dataset_id
                    || draft.label_schema_digest != plan.label_schema_digest
                    || draft.source_digest != reference.source_digest
                {
                    return Err(VideoAnnotationError::InvalidInput);
                }
                let mut coco = draft.coco_document;
                let local_image_id = coco["images"][0]["id"]
                    .as_i64()
                    .ok_or(VideoAnnotationError::InvalidInput)?;
                coco["images"][0]["file_name"] = json!(&filename);
                let bytes =
                    serde_json::to_vec(&coco).map_err(|_| VideoAnnotationError::InvalidInput)?;
                let relative_path = format!("{prefix}/frame-{}.json", frame.frame_index);
                Ok(PreparedAnnotation {
                    sha256: digest(&bytes),
                    bytes,
                    relative_path,
                    local_image_id,
                })
            })
            .transpose()?;
        let source_metadata = json!({
            "kind":"video_frame",
            "frame": {
                "source_video_id": frame.source_video_id, "source_digest": reference.source_digest,
                "stream_index": frame.stream_index, "frame_index": reference.frame_index,
                "pts_us": reference.pts_us, "pts": frame.raw_pts.map(|value|value.to_string()),
                "time_base_num": clock.numerator, "time_base_den": clock.denominator,
                "timestamp_ms": frame.pts_us.map(|value|value / 1000),
            },
            "sampling": {"dataset": config},
            "segment_id": plan.request.segment_id,
            "segment_revision": plan.segment_revision.to_string(),
            "extraction_method":"frame_resolve.v1",
            "extraction_task_id":task,
            "extraction_attempt_id":attempt,
        })
        .to_string();
        Ok(Self {
            frame,
            image_relative: resolved.relative_path,
            image_sha256: resolved.reference.sha256,
            image_size: resolved.reference.size_bytes,
            target_relative: format!("{prefix}/{filename}"),
            filename,
            annotation,
            source_metadata,
        })
    }

    fn publish(&self, root: &Path) -> Result<(), VideoAnnotationError> {
        if self.image_size == 0 || self.image_size > MAX_FRAME_BYTES {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let mut png = Vec::new();
        open_scoped_file(root, &self.image_relative)?
            .take(self.image_size + 1)
            .read_to_end(&mut png)?;
        if png.len() as u64 != self.image_size || digest(&png) != self.image_sha256 {
            return Err(VideoAnnotationError::InvalidInput);
        }
        publish_once(root, Path::new(&self.target_relative), &png)?;
        if let Some(annotation) = &self.annotation {
            publish_once(
                root,
                Path::new(&annotation.relative_path),
                &annotation.bytes,
            )?;
        }
        Ok(())
    }

    fn register(
        &self,
        tx: &Transaction<'_>,
        dataset: Uuid,
        video: Uuid,
        now: i64,
    ) -> Result<StagedVideoSample, WorkspaceThreadError> {
        let staged = tx.query_row(
            "SELECT a.asset_id,a.media_digest,a.source_video_id,a.file_origin,s.target_dataset_id,m.dataset_id
             FROM assets a LEFT JOIN staging_items s USING(asset_id)
             LEFT JOIN dataset_assets m USING(asset_id)
             WHERE a.location_kind='WORKSPACE' AND a.path=?1",
            [&self.target_relative],
            |row| Ok((row_uuid(row,0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?,
                row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?)),
        ).optional().map_err(database)?;
        let target = dataset.to_string();
        if let Some((asset_id, proof, source, origin, staging_target, member_target)) = staged {
            let same_target =
                staging_target.as_deref() == Some(target.as_str()) && member_target.is_none();
            let already_in_dataset =
                member_target.as_deref() == Some(target.as_str()) && staging_target.is_none();
            if origin != "GENERATED"
                || source.as_deref() != Some(video.to_string().as_str())
                || proof.as_deref() != Some(&self.image_sha256)
                || !(same_target || already_in_dataset)
            {
                return Err(invalid_storage());
            }
            return Ok(StagedVideoSample {
                asset_id,
                frame_index: self.frame.frame_index,
                already_in_dataset,
            });
        }
        let asset_id = Uuid::new_v4();
        let annotation = self.annotation.as_ref();
        tx.execute(
            "INSERT INTO assets(asset_id,kind,location_kind,path,original_filename,file_origin,
                 media_digest,annotation_path,annotation_format,annotation_origin,annotation_local_image_id,
                 annotation_digest,source_video_id,source_metadata_json,created_at_ms,updated_at_ms)
             VALUES(?1,'IMAGE','WORKSPACE',?2,?3,'GENERATED',?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
            params![asset_id.to_string(),self.target_relative,self.filename,self.image_sha256,
                annotation.map(|value|value.relative_path.as_str()),annotation.map(|_|"COCO_IMAGE_BBOX"),
                if annotation.is_some() {"APP_CREATED"} else {"NONE"},
                annotation.map(|value|value.local_image_id),annotation.map(|value|value.sha256.as_str()),
                video.to_string(),self.source_metadata,now],
        ).map_err(database)?;
        tx.execute(
            "INSERT INTO staging_items(asset_id,target_dataset_id,created_at_ms) VALUES(?1,?2,?3)",
            params![asset_id.to_string(), target, now],
        )
        .map_err(database)?;
        Ok(StagedVideoSample {
            asset_id,
            frame_index: self.frame.frame_index,
            already_in_dataset: false,
        })
    }
}

fn publish_once(root: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    match open_scoped_file(root, path) {
        Ok(file) => {
            let mut existing = Vec::new();
            file.take(bytes.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            if existing == bytes {
                return Ok(());
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Different bytes at sampled frame path",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            publish_immutable(root, path, bytes)
        }
        Err(error) => Err(error),
    }
}
