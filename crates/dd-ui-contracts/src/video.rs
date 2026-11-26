use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{UuidText, dataset_import::ImportOperationRef, workspace::CommandContext};

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
