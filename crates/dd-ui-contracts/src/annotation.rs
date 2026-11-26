use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ArtifactReference, DecimalU64, UuidText, dataset::AnnotationState, video::TrackObservationView,
    workspace::CommandContext,
};

#[derive(Deserialize, TS)]
pub struct AnnotationGetRequest {
    pub context: CommandContext,
    pub asset_id: UuidText,
}

#[derive(Debug, Deserialize, Serialize, TS)]
pub struct CocoDocument {
    pub images: Vec<CocoImage>,
    pub annotations: Vec<CocoBox>,
    pub categories: Vec<CocoCategory>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
pub struct CocoImage {
    #[ts(type = "number")]
    pub id: u64,
    pub file_name: String,
    #[ts(type = "number")]
    pub width: u64,
    #[ts(type = "number")]
    pub height: u64,
}

#[derive(Debug, Deserialize, Serialize, TS)]
pub struct CocoBox {
    #[ts(type = "number")]
    pub id: u64,
    #[ts(type = "number")]
    pub image_id: u64,
    #[ts(type = "number")]
    pub category_id: u64,
    pub bbox: [f64; 4],
    pub area: f64,
    pub iscrowd: u8,
}

#[derive(Debug, Deserialize, Serialize, TS)]
pub struct CocoCategory {
    #[ts(type = "number")]
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Serialize, TS)]
pub struct AnnotationView {
    pub asset_id: UuidText,
    pub image_digest: String,
    pub annotation_digest: Option<String>,
    pub formal_coco: Option<CocoDocument>,
    pub annotation_state: AnnotationState,
    pub active_review: Option<ReviewSession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tracking_candidates: Option<StagingTrackingCandidatesView>,
}

#[derive(Debug, Serialize, TS)]
#[serde(tag = "status", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StagingTrackingCandidatesView {
    Available {
        tracks_ref: ArtifactReference,
        frame_index: DecimalU64,
        observations: Vec<TrackObservationView>,
    },
    Unavailable,
}

#[derive(Debug, Serialize, TS)]
pub struct ReviewSession {
    pub review_id: UuidText,
    pub asset_id: UuidText,
    pub status: String,
    pub row_revision: DecimalU64,
    pub base_image_digest: String,
    pub base_annotation_digest: Option<String>,
    pub base_label_schema_digest: String,
}

#[derive(Deserialize, TS)]
pub struct ReviewOpenRequest {
    pub context: CommandContext,
    pub dataset_id: UuidText,
    pub asset_id: UuidText,
    pub base_image_digest: String,
    pub base_annotation_digest: Option<String>,
}

#[derive(Deserialize, TS)]
pub struct ReviewSaveDraftRequest {
    pub context: CommandContext,
    pub review_id: UuidText,
    pub source_task_id: UuidText,
    pub expected_review_revision: DecimalU64,
    pub expected_draft_revision: Option<DecimalU64>,
    pub coco_document: CocoDocument,
    #[ts(optional)]
    pub track_selections: Option<Vec<ReviewTrackSelection>>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewTrackSelection {
    #[ts(type = "number")]
    pub local_annotation_id: u64,
    pub track_id: UuidText,
}

#[derive(Deserialize, TS)]
pub struct ReviewCancelRequest {
    pub context: CommandContext,
    pub review_id: UuidText,
    pub expected_revision: DecimalU64,
}

#[derive(Deserialize, TS)]
pub struct ReviewCommitRequest {
    pub context: CommandContext,
    pub review_id: UuidText,
    pub expected_review_revision: DecimalU64,
    pub base_image_digest: String,
    pub base_annotation_digest: Option<String>,
    pub draft_digest: String,
    pub confirmed_negative: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct DraftReceipt {
    pub review_id: UuidText,
    pub draft_revision: DecimalU64,
    pub draft_digest: String,
    pub review_revision: DecimalU64,
}

#[derive(Debug, Serialize, TS)]
pub struct ReviewDraftView {
    pub receipt: DraftReceipt,
    pub source_task_id: UuidText,
    pub coco_document: CocoDocument,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub track_selections: Option<Vec<ReviewTrackSelection>>,
}

#[derive(Debug, Serialize, TS)]
pub struct ReviewEditorView {
    pub review: ReviewSession,
    pub categories: Vec<CocoCategory>,
    pub draft: Option<ReviewDraftView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub initial_track_selections: Option<Vec<ReviewTrackSelection>>,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaPurpose {
    Display,
}

#[derive(Deserialize, TS)]
pub struct MediaGetLocatorRequest {
    pub context: CommandContext,
    #[serde(default)]
    #[ts(optional)]
    pub asset_id: Option<UuidText>,
    #[serde(default)]
    #[ts(optional)]
    pub artifact_id: Option<UuidText>,
    #[serde(default)]
    #[ts(optional)]
    pub owner_task_id: Option<UuidText>,
    pub purpose: MediaPurpose,
}

#[derive(Debug, Serialize, TS)]
pub struct DisplayLocator {
    pub locator: String,
    pub media_type: String,
    pub expires_at: String,
    pub resource_digest: String,
}
