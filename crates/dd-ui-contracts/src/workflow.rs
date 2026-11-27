use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::staging::{StagingReportCheckView, StagingReportGetRequest, StagingReportView};
use crate::workspace::CommandContext;
use crate::{ArtifactReference, DecimalU64, UuidText};

#[derive(Clone, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStartRequest {
    pub context: CommandContext,
    pub scope: WorkflowDatasetScope,
    pub template_id: String,
    #[serde(default)]
    pub policy_overrides: WorkflowPolicyOverrides,
}

#[derive(Clone, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDatasetScope {
    pub dataset_id: UuidText,
}

#[derive(Clone, Default, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPolicyOverrides {
    #[ts(optional)]
    pub sample_limit: Option<u32>,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowGetRequest {
    pub context: CommandContext,
    pub workflow_run_id: UuidText,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPauseRequest {
    pub context: CommandContext,
    pub workflow_run_id: UuidText,
    pub expected_revision: DecimalU64,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowResumeRequest {
    pub context: CommandContext,
    pub workflow_run_id: UuidText,
    pub expected_revision: DecimalU64,
    pub strategy: WorkflowResumeStrategy,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowResumeStrategy {
    ResumePaused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowControlAction {
    Pause,
    Resume,
}

#[derive(Debug, Serialize, TS)]
pub struct WorkflowControlReceipt {
    pub workflow_run_id: UuidText,
    pub requested: WorkflowControlAction,
    pub observed_status: WorkflowStatusView,
    pub revision: DecimalU64,
    pub blocking_task_ids: Vec<UuidText>,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowListRequest {
    pub context: CommandContext,
    pub dataset_id: UuidText,
    #[ts(optional)]
    pub cursor: Option<UuidText>,
    pub page_size: u16,
}

#[derive(Debug, Serialize, TS)]
pub struct WorkflowPageView {
    pub items: Vec<WorkflowView>,
    pub next_cursor: Option<UuidText>,
}

#[derive(Debug, Serialize, TS)]
pub struct WorkflowView {
    pub workflow_run_id: UuidText,
    pub planning_task_id: UuidText,
    pub status: WorkflowStatusView,
    pub row_revision: DecimalU64,
    pub plan_revision: Option<DecimalU64>,
    pub input_fingerprint: Option<String>,
    pub plan_source: Option<WorkflowPlanSourceView>,
    pub profile_task: Option<WorkflowTaskView>,
    pub geometry_tasks: Vec<WorkflowGeometryTaskView>,
    pub report_task_id: Option<UuidText>,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowStatusView {
    Draft,
    Ready,
    Running,
    Paused,
    Interrupted,
    WaitingReview,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowPlanSourceView {
    Proposed,
    LocalDefault,
}

#[derive(Debug, Serialize, TS)]
pub struct WorkflowTaskView {
    pub task_id: UuidText,
    pub status: String,
}

#[derive(Debug, Serialize, TS)]
pub struct WorkflowGeometryTaskView {
    pub asset_id: UuidText,
    pub task_id: UuidText,
    pub status: String,
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct WorkflowReportGetRequest {
    pub context: CommandContext,
    pub workflow_run_id: UuidText,
}

#[derive(Deserialize, TS)]
#[serde(untagged)]
pub enum ReportGetRequest {
    Workflow(WorkflowReportGetRequest),
    Staging(StagingReportGetRequest),
}

#[derive(Debug, Serialize, TS)]
#[serde(untagged)]
pub enum ReportGetView {
    Workflow(WorkflowReportView),
    Staging(StagingReportView),
}

#[derive(Debug, Serialize, TS)]
pub struct WorkflowReportView {
    pub report_id: UuidText,
    pub scope: WorkflowReportScopeView,
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
pub struct WorkflowReportScopeView {
    pub dataset_id: UuidText,
    pub workflow_run_id: UuidText,
}
