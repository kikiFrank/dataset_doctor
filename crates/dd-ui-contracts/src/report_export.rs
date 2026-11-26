//! Report export requests and receipts exposed to the desktop.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::workspace::CommandContext;
use crate::{ArtifactReference, DecimalU64, UuidText};

#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReportExportFormat {
    Json,
    Markdown,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReportExportSubmitRequest {
    pub context: CommandContext,
    pub report_ref: ArtifactReference,
    pub format: ReportExportFormat,
    pub destination_grant_id: UuidText,
}

#[derive(Debug, Serialize, TS)]
pub struct ReportExportView {
    pub report_ref: ArtifactReference,
    pub format: ReportExportFormat,
    pub file_name: String,
    pub sha256: String,
    pub size_bytes: DecimalU64,
}
