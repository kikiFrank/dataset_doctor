use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ArtifactReference, DecimalU64, UuidText, video::VideoFrame, workspace::CommandContext,
};

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

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StagingReportGetRequest {
    pub context: CommandContext,
    #[ts(optional)]
    pub report_id: Option<UuidText>,
    #[ts(optional)]
    pub task_id: Option<UuidText>,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingReportView {
    pub report_id: UuidText,
    pub scope: StagingReportScopeView,
    pub input_fingerprint: String,
    pub execution_coverage: BTreeMap<String, u32>,
    pub findings_by_severity: BTreeMap<String, u32>,
    pub human_ack_status: String,
    pub unexecuted_checks: Vec<String>,
    #[ts(type = "unknown[]")]
    pub repair_outcomes: Vec<serde_json::Value>,
    pub checks: Vec<StagingReportCheckView>,
    pub report_artifact_ref: ArtifactReference,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingReportScopeView {
    pub dataset_id: UuidText,
    pub asset_id: UuidText,
    pub media_digest: String,
    pub annotation_digest: Option<String>,
    pub label_schema_digest: String,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingReportCheckView {
    pub check_id: String,
    pub check_contract_version: String,
    pub execution_status: String,
    pub assessment: String,
    pub skip_or_failure_reason: Option<StagingReportReasonView>,
    pub input_fingerprint: String,
    pub processed_assets: u32,
    pub total_assets: u32,
    #[ts(type = "number")]
    pub finding_count: usize,
    pub findings: Vec<StagingReportFindingView>,
    pub evidence_refs: Vec<ArtifactReference>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingReportReasonView {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingReportFindingView {
    pub issue_id: UuidText,
    pub asset_id: UuidText,
    pub check_id: String,
    pub finding_code: String,
    pub severity: String,
    pub input_fingerprint: String,
    pub explanation: String,
    pub annotation_id: Option<DecimalU64>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StagingFindingAck {
    pub issue_id: UuidText,
    #[ts(optional)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StagingIncompleteCheckAck {
    pub check_id: String,
    pub reason: String,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StagingCommitRequest {
    pub context: CommandContext,
    pub target_dataset_id: UuidText,
    pub asset_ids: Vec<UuidText>,
    pub quality_report_ref: ArtifactReference,
    pub expected_schema_fingerprint: String,
    pub acknowledged_findings: Vec<StagingFindingAck>,
    #[ts(optional)]
    pub acknowledged_incomplete_checks: Option<Vec<StagingIncompleteCheckAck>>,
}

#[derive(Debug, Serialize, TS)]
pub struct StagingCommitView {
    pub target_dataset_id: UuidText,
    pub accepted_asset_ids: Vec<UuidText>,
    pub quality_report_ref: ArtifactReference,
    pub acknowledged_findings: Vec<StagingFindingAck>,
    pub acknowledged_incomplete_checks: Vec<StagingIncompleteCheckAck>,
}
