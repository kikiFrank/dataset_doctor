use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DecimalU64, UuidText, dataset_import::ImportOperationRef, workspace::CommandContext};

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
}
