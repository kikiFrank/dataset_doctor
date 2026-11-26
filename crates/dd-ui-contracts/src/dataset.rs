use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DecimalI64, DecimalU64, UuidText, workspace::CommandContext};

#[derive(Deserialize, TS)]
pub struct DatasetListRequest {
    pub context: CommandContext,
    #[serde(default)]
    #[ts(optional)]
    pub page_cursor: Option<UuidText>,
    pub page_size: u16,
}

#[derive(Debug, Serialize, TS)]
pub struct DatasetSummary {
    pub dataset_id: UuidText,
    pub name: String,
    pub asset_count: DecimalU64,
    pub unconfirmed_count: DecimalU64,
    pub row_revision: DecimalU64,
    pub label_schema_digest: String,
    pub updated_at_ms: DecimalI64,
}

#[derive(Debug, Serialize, TS)]
pub struct DatasetPage {
    pub items: Vec<DatasetSummary>,
    pub next_cursor: Option<UuidText>,
}

#[derive(Deserialize, TS)]
pub struct AssetListRequest {
    pub context: CommandContext,
    #[serde(default)]
    #[ts(optional)]
    pub dataset_id: Option<UuidText>,
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<AssetKind>,
    #[serde(default)]
    #[ts(optional)]
    pub cursor: Option<UuidText>,
    pub page_size: u16,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetKind {
    Image,
    Video,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AnnotationState {
    Unannotated,
    Unreviewed,
    ConfirmedPositive,
    ConfirmedNegative,
    Stale,
}

#[derive(Debug, Serialize, TS)]
pub struct AssetSummary {
    pub asset_id: UuidText,
    pub dataset_id: Option<UuidText>,
    pub display_name: String,
    pub kind: String,
    pub availability: String,
    pub annotation_state: AnnotationState,
    pub active_review: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct AssetPage {
    pub items: Vec<AssetSummary>,
    pub next_cursor: Option<UuidText>,
}
