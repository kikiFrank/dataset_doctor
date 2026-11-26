use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DecimalU64, UuidText, workspace::CommandContext};

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DatasetExportFormat {
    CocoDetection,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DatasetExportSelection {
    AllMembers,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExportPolicy {
    Strict,
    ExcludeIneligible,
}

#[derive(Deserialize, TS)]
pub struct DatasetExportSubmitRequest {
    pub context: CommandContext,
    pub dataset_id: UuidText,
    pub destination_grant_id: UuidText,
    pub format: DatasetExportFormat,
    pub selection: DatasetExportSelection,
    pub export_policy: ExportPolicy,
}

#[derive(Debug, Serialize, TS)]
pub struct CocoExportView {
    pub package_path: String,
    pub image_count: DecimalU64,
    pub annotation_count: DecimalU64,
    pub excluded_count: DecimalU64,
}
