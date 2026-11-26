use std::collections::BTreeSet;
use std::sync::Arc;

use uuid::Uuid;

use super::{ResolvedVideoSampling, VideoAnnotationError};
use crate::task_store::format_digest;
use crate::video_annotation_store::{VideoArtifactOwner, VideoArtifactReference};
use crate::video_tracking_runtime::VideoTrackingService;
use crate::workspace_thread::WorkspaceThread;

pub(super) fn candidate_frames(
    owner: &Arc<WorkspaceThread>,
    session: Uuid,
    plan: &ResolvedVideoSampling,
    reference: &VideoArtifactReference,
) -> Result<BTreeSet<u64>, VideoAnnotationError> {
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
    let data = bundle.data;
    if &bundle.reference != reference
        || bundle.target_dataset_id != plan.request.target_dataset_id
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
