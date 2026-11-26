use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ArtifactReference, DecimalU64, UuidText, workspace::CommandContext};

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ColorMethod {
    HueRemap,
    SolidFill,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ColorRegion {
    #[serde(rename = "RECT")]
    Rectangle { bbox_xywh: [f64; 4] },
    #[serde(rename = "POLYGON")]
    Polygon { points_xy: Vec<[f64; 2]> },
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ColorHueGate {
    pub hue_min_deg: f64,
    pub hue_max_deg: f64,
    pub wraps_zero: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ColorOptions {
    pub method: ColorMethod,
    pub target_rgb: [u8; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub strength: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feather_px: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_color_gate: Option<ColorHueGate>,
}

#[derive(Deserialize, TS)]
pub struct ColorPreviewSubmitRequest {
    pub context: CommandContext,
    pub dataset_id: UuidText,
    pub asset_id: UuidText,
    pub base_image_digest: String,
    pub region: ColorRegion,
    pub operation: ColorOptions,
}

#[derive(Deserialize, TS)]
pub struct ColorApplySubmitRequest {
    pub context: CommandContext,
    pub dataset_id: UuidText,
    pub asset_id: UuidText,
    pub base_image_digest: String,
    pub expected_asset_revision: DecimalU64,
    pub candidate_ref: ArtifactReference,
}

#[derive(Debug, Serialize, TS)]
pub struct ColorPreviewView {
    pub dataset_id: UuidText,
    pub asset_id: UuidText,
    pub asset_revision: DecimalU64,
    pub base_image_digest: String,
    pub candidate_ref: ArtifactReference,
    pub mask_ref: ArtifactReference,
    pub changed_pixels: DecimalU64,
    pub lossy_reencode: bool,
    pub region: ColorRegion,
    pub operation: ColorOptions,
}

#[derive(Debug, Serialize, TS)]
pub struct ColorApplyView {
    pub operation_id: UuidText,
    pub asset_id: UuidText,
    pub image_digest: String,
}
