use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ArtifactReference, DecimalI64, DecimalU64, UuidText, annotation::CocoDocument,
    dataset_import::ImportOperationRef, workspace::CommandContext,
};

#[derive(Deserialize, TS)]
pub struct VideoImportRequest {
    pub context: CommandContext,
    pub file_grant_id: UuidText,
    pub display_name: Option<String>,
    pub stream_index: Option<u32>,
}

#[derive(Serialize, TS)]
pub struct VideoImportView {
    pub source_video_id: UuidText,
    pub operation: ImportOperationRef,
}

#[derive(Deserialize, TS)]
pub struct VideoGetRequest {
    pub context: CommandContext,
    pub source_video_id: UuidText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VideoAvailability {
    Online,
    Offline,
    Changed,
}

#[derive(Serialize, TS)]
pub struct VideoStreamView {
    pub stream_index: u32,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<DecimalU64>,
}

#[derive(Serialize, TS)]
pub struct VideoDetail {
    pub source_video_id: UuidText,
    pub display_name: String,
    pub row_revision: DecimalU64,
    pub source_digest: String,
    pub source_availability: VideoAvailability,
    pub probe_status: Option<String>,
    pub duration_ms: Option<DecimalU64>,
    pub streams: Vec<VideoStreamView>,
    pub rotation_ccw: Option<u16>,
    pub variable_frame_rate: Option<bool>,
    pub video_annotation_manifest_ref: Option<ArtifactReference>,
    pub video_annotation_manifest: Option<VideoAnnotationManifest>,
}

#[derive(Deserialize, TS)]
#[serde(untagged, deny_unknown_fields)]
pub enum VideoFrameTarget {
    Index { frame_index: DecimalU64 },
    Timestamp { timestamp_ms: DecimalU64 },
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VideoSeekPolicy {
    Nearest,
    AtOrAfter,
}

#[derive(Deserialize, TS)]
pub struct VideoResolveFrameRequest {
    pub context: CommandContext,
    pub source_video_id: UuidText,
    pub target: VideoFrameTarget,
    pub seek_policy: VideoSeekPolicy,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VideoFrame {
    pub source_video_id: UuidText,
    pub source_digest: String,
    pub stream_index: u32,
    pub frame_index: DecimalU64,
    pub pts_us: Option<crate::DecimalI64>,
    pub timestamp_ms: Option<DecimalU64>,
    pub width: u32,
    pub height: u32,
    pub image_ref: crate::ArtifactReference,
}

#[derive(Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VideoSaveKeyframeDraftRequest {
    pub context: CommandContext,
    pub source_video_id: UuidText,
    pub target_dataset_id: UuidText,
    pub frame_ref: VideoFrame,
    pub coco_document: CocoDocument,
    pub expected_video_revision: DecimalU64,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VideoGetKeyframeDraftRequest {
    pub context: CommandContext,
    pub source_video_id: UuidText,
    pub annotation_ref: ArtifactReference,
}

#[derive(Debug, Serialize, TS)]
pub struct KeyframeDraftReceipt {
    pub source_video_id: UuidText,
    pub target_dataset_id: UuidText,
    pub frame_ref: VideoFrame,
    pub annotation_ref: ArtifactReference,
    pub draft_digest: String,
    pub video_revision: DecimalU64,
}

#[derive(Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VideoSaveSegmentRequest {
    pub context: CommandContext,
    pub source_video_id: UuidText,
    pub target_dataset_id: UuidText,
    #[ts(optional)]
    pub segment_id: Option<UuidText>,
    pub start_us: DecimalI64,
    pub end_us: DecimalI64,
    pub keyframes: Vec<VideoKeyframe>,
    pub expected_revision: DecimalU64,
}

#[derive(Debug, Serialize, TS)]
pub struct SegmentReceipt {
    pub segment_id: UuidText,
    pub revision: DecimalU64,
    pub video_annotation_manifest_ref: ArtifactReference,
    pub validation_warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VideoKeyframe {
    pub frame_ref: VideoFrame,
    pub annotation_ref: ArtifactReference,
}

#[derive(Deserialize, Serialize, TS)]
pub struct UnassignedVideoKeyframe {
    pub target_dataset_id: UuidText,
    pub frame_ref: VideoFrame,
    pub annotation_ref: ArtifactReference,
}

#[derive(Deserialize, Serialize, TS)]
pub struct VideoSegment {
    pub segment_id: UuidText,
    pub source_video_id: UuidText,
    pub source_digest: String,
    pub start_us: DecimalI64,
    pub end_us: DecimalI64,
    pub keyframes: Vec<VideoKeyframe>,
    pub target_dataset_id: UuidText,
    pub revision: DecimalU64,
}

#[derive(Deserialize, Serialize, TS)]
pub struct VideoAnnotationManifest {
    pub contract_version: u8,
    pub source_video_id: UuidText,
    pub source_digest: String,
    pub target_contexts: std::collections::BTreeMap<String, VideoTargetContext>,
    pub segments: Vec<VideoSegment>,
    pub unassigned_keyframe_drafts: Vec<UnassignedVideoKeyframe>,
}

#[derive(Deserialize, Serialize, TS)]
pub struct VideoTargetContext {
    pub label_schema_digest: String,
}

#[derive(Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameResolution {
    Ready { frame: VideoFrame },
    Accepted { operation: ImportOperationRef },
}
