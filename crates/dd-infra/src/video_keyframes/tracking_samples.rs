use std::collections::BTreeSet;
use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use uuid::Uuid;

use super::{ResolvedVideoSampling, VideoAnnotationError, VideoAnnotationService, database};
use crate::frame_runtime::FrameService;
use crate::sqlite::row_uuid;
use crate::staging::parse_source;
use crate::task_store::format_digest;
use crate::video_annotation_store::{
    VideoArtifactOwner, VideoArtifactReference, VideoFrameReference,
};
use crate::video_tracking_runtime::{TrackObservation, VideoTrackBundle, VideoTrackingService};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

#[derive(Debug)]
pub struct StagingTrackCandidates {
    pub reference: VideoArtifactReference,
    pub tracking_attempt_id: Uuid,
    pub frame_index: u64,
    pub segment_id: Uuid,
    pub frame: VideoFrameReference,
    pub observations: Vec<TrackObservation>,
}

#[derive(Deserialize)]
struct CandidatePointer {
    tracking_candidate_ref: VideoArtifactReference,
}

impl VideoAnnotationService {
    /// Read the verified candidates for this managed staged image, without changing its labels.
    pub fn staging_tracking_candidates(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
    ) -> Result<Option<StagingTrackCandidates>, VideoAnnotationError> {
        let _guard = self.owner.lock_final_operations()?;
        self.staging_tracking_candidates_locked(session, dataset, asset)
    }

    pub(crate) fn staging_tracking_candidates_locked(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
    ) -> Result<Option<StagingTrackCandidates>, VideoAnnotationError> {
        let owner = &self.owner;
        let candidate = owner.workspace_action(session, move |workspace| {
            let record = workspace
                .connection
                .query_row(
                    "SELECT a.source_video_id, a.source_metadata_json
                 FROM staging_items s JOIN assets a USING(asset_id)
                 WHERE s.asset_id=?1 AND s.target_dataset_id=?2
                   AND a.file_origin='GENERATED' AND a.location_kind='WORKSPACE'
                   AND json_type(a.source_metadata_json,'$.tracking_candidate_ref') IS NOT NULL",
                    params![asset.to_string(), dataset.to_string()],
                    |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(database)?;
            record
                .map(|(video, raw)| {
                    let image = workspace
                        .registered_image(dataset, asset)
                        .map_err(WorkspaceThreadError::Dataset)?;
                    Ok((video, raw, image))
                })
                .transpose()
        })?;
        let Some((video, raw, image)) = candidate else {
            return Ok(None);
        };
        let invalid = || VideoAnnotationError::TrackingEvidenceInvalid;
        let source = parse_source(video, &raw).ok_or_else(invalid)?;
        let pointer: CandidatePointer = serde_json::from_str(&raw).map_err(|_| invalid())?;
        let reference = pointer.tracking_candidate_ref;
        let frame_task = source.extraction_task_id.ok_or_else(invalid)?;
        let frames = FrameService::new(Arc::clone(owner)).map_err(|_| invalid())?;
        let resolved = frames
            .snapshot(session, frame_task)
            .map_err(|_| invalid())?
            .frame
            .ok_or_else(invalid)?;
        let frame = resolved.frame;
        if frame.source_video_id != video
            || format_digest(&frame.source_digest) != source.source_digest
            || frame.stream_index != source.stream_index
            || frame.frame_index != source.frame_index
            || frame.pts_us != source.pts_us
            || resolved.reference.sha256 != image.image.sha256
            || !image.image.live_digest(128 * 1024 * 1024)?
        {
            return Err(invalid());
        }
        let bundle = verified_bundle(owner, session, &reference)?;
        let data = bundle.data;
        if bundle.target_dataset_id != dataset
            || bundle.label_schema_digest != image.label_schema_fingerprint
            || data.source_video_id != video
            || data.source_digest != source.source_digest
            || data.stream_index != source.stream_index
        {
            return Err(invalid());
        }
        let frame_index = source.frame_index.to_string();
        let observations: Vec<_> = data
            .observations
            .into_iter()
            .filter(|observation| {
                observation.source == "TRACKER" && observation.frame_index == frame_index
            })
            .collect();
        if observations.is_empty() {
            return Err(invalid());
        }
        Ok(Some(StagingTrackCandidates {
            reference,
            tracking_attempt_id: bundle.attempt_id,
            frame_index: source.frame_index,
            segment_id: data.segment_id,
            frame: VideoFrameReference::from_resolved(frame_task, &resolved)?,
            observations,
        }))
    }
}

pub(super) fn candidate_frames(
    owner: &Arc<WorkspaceThread>,
    session: Uuid,
    plan: &ResolvedVideoSampling,
    reference: &VideoArtifactReference,
) -> Result<BTreeSet<u64>, VideoAnnotationError> {
    let invalid = || VideoAnnotationError::TrackingEvidenceInvalid;
    let bundle = verified_bundle(owner, session, reference)?;
    let data = bundle.data;
    if bundle.target_dataset_id != plan.request.target_dataset_id
        || bundle.segment_revision != plan.segment_revision
        || bundle.label_schema_digest != plan.label_schema_digest
        || data.source_video_id != plan.request.source_video_id
        || data.source_digest != format_digest(&plan.source.source_digest)
        || data.segment_id != plan.request.segment_id
        || data.stream_index != plan.source.stream_index
    {
        return Err(invalid());
    }
    data.observations
        .into_iter()
        .filter(|item| item.source == "TRACKER")
        .map(|item| item.frame_index.parse::<u64>().map_err(|_| invalid()))
        .collect()
}

fn verified_bundle(
    owner: &Arc<WorkspaceThread>,
    session: Uuid,
    reference: &VideoArtifactReference,
) -> Result<VideoTrackBundle, VideoAnnotationError> {
    let invalid = || VideoAnnotationError::TrackingEvidenceInvalid;
    let VideoArtifactOwner::Task(task) = reference.owner else {
        return Err(invalid());
    };
    let tracker = VideoTrackingService::new(Arc::clone(owner)).map_err(|_| invalid())?;
    let bundle = tracker
        .snapshot(session, task)
        .map_err(|_| invalid())?
        .bundle
        .ok_or_else(invalid)?;
    if &bundle.reference != reference {
        return Err(invalid());
    }
    Ok(bundle)
}
