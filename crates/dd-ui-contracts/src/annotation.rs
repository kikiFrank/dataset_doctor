use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DecimalU64, UuidText, dataset::AnnotationState, workspace::CommandContext};

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
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaPurpose {
    Display,
}

#[derive(Deserialize, TS)]
pub struct MediaGetLocatorRequest {
    pub context: CommandContext,
    pub asset_id: UuidText,
    pub purpose: MediaPurpose,
}

#[derive(Debug, Serialize, TS)]
pub struct DisplayLocator {
    pub locator: String,
    pub media_type: String,
    pub expires_at: String,
    pub resource_digest: String,
}
