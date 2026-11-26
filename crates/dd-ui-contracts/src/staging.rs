use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ArtifactReference, UuidText, video::VideoFrame, workspace::CommandContext};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StagingState {
    Offline,
    Conflict,
    Accepting,
    Checking,
    NeedsAnnotation,
    NeedsReview,
    NeedsCheck,
    ReadyForAcceptance,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StagingListRequest {
    pub context: CommandContext,
    #[ts(optional)]
    pub target_dataset_id: Option<UuidText>,
    #[ts(optional)]
    pub cursor: Option<UuidText>,
    #[ts(optional)]
    pub status: Option<StagingState>,
    pub page_size: u16,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingItem {
    pub asset_id: UuidText,
    pub target_dataset_id: Option<UuidText>,
    pub display_name: String,
    pub state: StagingState,
    pub review_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_report_ref: Option<ArtifactReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_frame_ref: Option<VideoFrame>,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingPage {
    pub items: Vec<StagingItem>,
    pub next_cursor: Option<UuidText>,
}
