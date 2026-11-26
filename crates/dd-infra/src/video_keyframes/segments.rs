use std::collections::{BTreeMap, BTreeSet};

use dd_domain::video::SeekPolicy;
use uuid::Uuid;

use super::{VideoAnnotationError, VideoAnnotationService, database, digest};
use crate::task_store::parse_digest;
use crate::video_annotation_store::{
    UnassignedKeyframe, VideoAnnotationManifest, VideoArtifactReference, VideoKeyframe,
    VideoSegment, VideoTargetContext,
};
use crate::video_query::{VideoSourceAvailability, registered_video_detail};
use crate::video_runtime::read_registered_video_index;

#[derive(Clone)]
pub struct SegmentRequest {
    pub source_video_id: Uuid,
    pub target_dataset_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub start_us: i64,
    pub end_us: i64,
    pub keyframes: Vec<VideoKeyframe>,
    pub expected_revision: u64,
}

#[derive(Debug)]
pub struct SegmentReceipt {
    pub segment_id: Uuid,
    pub revision: u64,
    pub video_annotation_manifest_ref: VideoArtifactReference,
    pub validation_warnings: Vec<String>,
}

impl VideoAnnotationService {
    pub fn save_segment(
        &self,
        session: Uuid,
        request: SegmentRequest,
        now_ms: i64,
    ) -> Result<SegmentReceipt, VideoAnnotationError> {
        if request.start_us < 0
            || request.end_us <= request.start_us
            || request.segment_id.is_some_and(|id| id.is_nil())
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let _guard = self.owner.lock_final_operations()?;
        let snapshot = self.snapshot(session, request.source_video_id)?;
        let detail = registered_video_detail(&self.owner, session, request.source_video_id)
            .map_err(|e| VideoAnnotationError::Evidence(Box::new(e)))?;
        if detail.source_availability != VideoSourceAvailability::Online
            || detail.source_digest != snapshot.digest
            || detail.probe_status.as_deref() != Some("SUCCEEDED")
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let index = read_registered_video_index(
            &self.owner,
            session,
            detail
                .probe_receipt
                .ok_or(VideoAnnotationError::InvalidInput)?,
        )
        .map_err(|e| VideoAnnotationError::Evidence(Box::new(e)))?;
        let source_digest =
            parse_digest(&snapshot.digest).ok_or(VideoAnnotationError::InvalidInput)?;
        index
            .resolve_time(source_digest, request.start_us, SeekPolicy::AtOrAfter)
            .map_err(|e| VideoAnnotationError::Evidence(Box::new(e)))?;
        let dataset = request.target_dataset_id;
        let categories = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    "SELECT categories_json FROM datasets WHERE dataset_id=?1",
                    [dataset.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .map_err(database)
        })?;
        let schema = digest(categories.as_bytes());
        let (manifest, _) = self.load_manifest(&snapshot, request.source_video_id)?;
        let mut manifest = manifest.unwrap_or_else(|| VideoAnnotationManifest {
            contract_version: 1,
            source_video_id: request.source_video_id,
            source_digest: snapshot.digest.clone(),
            target_contexts: BTreeMap::new(),
            segments: Vec::new(),
            unassigned_keyframe_drafts: Vec::new(),
        });
        if manifest
            .target_contexts
            .get(&dataset)
            .is_some_and(|context| context.label_schema_digest != schema)
        {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        let existing = request.segment_id.and_then(|id| {
            manifest
                .segments
                .iter()
                .position(|segment| segment.segment_id == id)
        });
        let previous = existing.map(|position| manifest.segments[position].clone());
        let revision = match &previous {
            Some(segment) if segment.revision == request.expected_revision.to_string() => {
                request.expected_revision.checked_add(1)
            }
            None if request.segment_id.is_none() && request.expected_revision == 0 => Some(1),
            _ => return Err(VideoAnnotationError::RevisionConflict),
        }
        .ok_or(VideoAnnotationError::InvalidInput)?;
        let mut frames = BTreeSet::new();
        for keyframe in &request.keyframes {
            let frame = &keyframe.frame_ref;
            let pts = frame
                .pts_us
                .as_deref()
                .and_then(|value| value.parse::<i64>().ok())
                .ok_or(VideoAnnotationError::InvalidInput)?;
            if frame.source_video_id != request.source_video_id
                || frame.source_digest != snapshot.digest
                || pts < request.start_us
                || pts >= request.end_us
                || !frames.insert((frame.stream_index, frame.frame_index.clone()))
                || !manifest.keyframes().any(|(target, frame, annotation)| {
                    target == dataset
                        && *frame == keyframe.frame_ref
                        && *annotation == keyframe.annotation_ref
                })
            {
                return Err(VideoAnnotationError::InvalidInput);
            }
            self.verify_frame(session, frame)?;
        }
        let segment_id = request.segment_id.unwrap_or_else(Uuid::new_v4);
        let segment = VideoSegment {
            segment_id,
            source_video_id: request.source_video_id,
            source_digest: snapshot.digest.clone(),
            start_us: request.start_us.to_string(),
            end_us: request.end_us.to_string(),
            keyframes: request.keyframes,
            target_dataset_id: dataset,
            revision: revision.to_string(),
        };
        if let Some(position) = existing {
            manifest.segments[position] = segment;
        } else {
            manifest.segments.push(segment);
        }
        manifest.target_contexts.insert(
            dataset,
            VideoTargetContext {
                label_schema_digest: schema,
            },
        );
        // Moving work between assigned and unassigned collections must not discard human drafts.
        if let Some(previous) = previous {
            for keyframe in previous.keyframes {
                if !manifest.keyframes().any(|(target, frame, annotation)| {
                    target == previous.target_dataset_id
                        && *frame == keyframe.frame_ref
                        && *annotation == keyframe.annotation_ref
                }) {
                    manifest
                        .unassigned_keyframe_drafts
                        .push(UnassignedKeyframe {
                            target_dataset_id: previous.target_dataset_id,
                            frame_ref: keyframe.frame_ref,
                            annotation_ref: keyframe.annotation_ref,
                        });
                }
            }
        }
        let assigned: Vec<_> = manifest
            .segments
            .iter()
            .flat_map(|segment| {
                segment
                    .keyframes
                    .iter()
                    .map(move |frame| (segment.target_dataset_id, frame.annotation_ref.artifact_id))
            })
            .collect();
        manifest.unassigned_keyframe_drafts.retain(|draft| {
            !assigned.contains(&(draft.target_dataset_id, draft.annotation_ref.artifact_id))
        });
        let (reference, _) = self.commit_manifest(
            session,
            request.source_video_id,
            snapshot,
            &manifest,
            (dataset, categories),
            now_ms,
        )?;
        Ok(SegmentReceipt {
            segment_id,
            revision,
            video_annotation_manifest_ref: reference,
            validation_warnings: Vec::new(),
        })
    }
}
