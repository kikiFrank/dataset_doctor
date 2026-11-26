//! Convert accepted local task results and declared coverage gaps into report checks.

use std::collections::{BTreeMap, BTreeSet};

use dd_core::workflow::{DefaultSkipReason, Step, TemplatePlan, template};
use serde_json::Value;
use uuid::Uuid;

use super::super::{RowData, WorkflowError};
use super::evidence;
use crate::dataset_profile_runtime::read_profile_evidence;
use crate::geometry_runtime::read_bound_geometry_report;
use crate::staging_report::{
    CheckReason, ReportCheck, ReportFinding, build_geometry_check, timestamp,
};
use crate::task_store::format_digest;
use crate::workspace_thread::WorkspaceThread;

const PROFILE: &str = "dataset_profile";
const GEOMETRY: &str = "annotation_geometry";

pub(super) fn collect(
    owner: &WorkspaceThread,
    session: Uuid,
    row: &RowData,
    plan: &TemplatePlan,
    skipped: &BTreeMap<Step, DefaultSkipReason>,
    scope: &Value,
) -> Result<Vec<ReportCheck>, WorkflowError> {
    let profile_task = row
        .profile_task
        .as_ref()
        .filter(|task| task.status == "SUCCEEDED")
        .ok_or(WorkflowError::UpstreamNotReady)?;
    let total_assets = scope["assets"]
        .as_array()
        .and_then(|assets| u32::try_from(assets.len()).ok())
        .ok_or(WorkflowError::InvalidRecord)?;
    let verified = read_profile_evidence(owner, session, row.dataset_id, profile_task.task_id)
        .map_err(evidence)?;
    let profile = verified.data;
    let bad_images: BTreeSet<_> = profile
        .file_errors
        .iter()
        .filter(|error| error.source == "image")
        .map(|error| error.asset_id)
        .collect();
    let processed_assets =
        u32::try_from(profile.asset_count.saturating_sub(bad_images.len())).map_err(evidence)?;
    let mut local = check(PROFILE, &profile.input_fingerprint, total_assets);
    local.processed_assets = processed_assets;
    local.execution_status = if profile.asset_count == 0 {
        "SKIPPED"
    } else {
        "COMPLETED"
    }
    .into();
    local.evidence_refs.push(verified.reference);
    local.started_at = Some(timestamp(verified.started_at_ms).map_err(evidence)?);
    local.finished_at = Some(timestamp(verified.finished_at_ms).map_err(evidence)?);
    for error in &profile.file_errors {
        local.findings.push(ReportFinding {
            issue_id: Uuid::new_v4(),
            asset_id: error.asset_id,
            check_id: PROFILE.into(),
            finding_code: error.code.clone(),
            severity: "WARNING".into(),
            input_fingerprint: profile.input_fingerprint.clone(),
            explanation: format!(
                "Could not process the {} input for this asset ({})",
                error.source, error.code
            ),
            annotation_id: None,
        });
    }
    local.finding_count = local.findings.len();
    local.assessment = if local.finding_count != 0 {
        "FINDINGS"
    } else if profile.asset_count == 0 {
        "NOT_EVALUATED"
    } else if processed_assets < total_assets {
        "INCONCLUSIVE"
    } else {
        "PASS"
    }
    .into();
    if processed_assets < total_assets || profile.asset_count == 0 {
        local.skip_or_failure_reason = Some(coverage_reason());
    }

    let mut geometry = check(
        GEOMETRY,
        &format_digest(&plan.expected_scope_fingerprint),
        total_assets,
    );
    let mut failed = false;
    let mut started_at: Option<i64> = None;
    let mut finished_at: Option<i64> = None;
    for task in &row.geometry_tasks {
        if task.status != "SUCCEEDED" {
            let outcome = owner
                .task_outcome(session, task.task_id)
                .map_err(WorkflowError::Storage)?;
            let code = outcome.error_code.unwrap_or_else(|| task.status.clone());
            geometry.findings.push(ReportFinding {
                issue_id: Uuid::new_v4(),
                asset_id: task.asset_id,
                check_id: GEOMETRY.into(),
                finding_code: code.clone(),
                severity: "WARNING".into(),
                input_fingerprint: geometry.input_fingerprint.clone(),
                explanation: format!("The geometry task ended without accepted evidence ({code})"),
                annotation_id: None,
            });
            failed = true;
            continue;
        }
        let image = owner
            .registered_image(session, row.dataset_id, task.asset_id)
            .map_err(WorkflowError::Storage)?;
        let checked =
            read_bound_geometry_report(owner, session, task.task_id, &image).map_err(evidence)?;
        started_at =
            Some(started_at.map_or(checked.started_at_ms, |old| old.min(checked.started_at_ms)));
        finished_at = Some(finished_at.map_or(checked.finished_at_ms, |old| {
            old.max(checked.finished_at_ms)
        }));
        let per_image = build_geometry_check(task.asset_id, checked).map_err(evidence)?;
        geometry.processed_assets += per_image.processed_assets;
        geometry.findings.extend(per_image.findings);
        geometry.evidence_refs.extend(per_image.evidence_refs);
    }
    geometry.started_at = started_at.map(timestamp).transpose().map_err(evidence)?;
    geometry.finished_at = finished_at.map(timestamp).transpose().map_err(evidence)?;
    geometry.finding_count = geometry.findings.len();
    if geometry.processed_assets != 0 {
        geometry.execution_status = "COMPLETED".into();
        geometry.assessment = if geometry
            .findings
            .iter()
            .any(|finding| matches!(finding.severity.as_str(), "ERROR" | "CRITICAL"))
        {
            "FINDINGS"
        } else if geometry.processed_assets < geometry.total_assets {
            "INCONCLUSIVE"
        } else {
            "PASS"
        }
        .into();
    } else if failed {
        geometry.execution_status = "FAILED".into();
    }
    if geometry.processed_assets < geometry.total_assets || geometry.total_assets == 0 {
        geometry.skip_or_failure_reason = Some(coverage_reason());
    }

    let mut checks = vec![local, geometry];
    let spec = template(&plan.template_id).ok_or(WorkflowError::InvalidRecord)?;
    for step in spec.optional_steps {
        let name = match step {
            Step::ImageQuality => "image_quality",
            Step::DetectorPredictions => "detector_predictions",
            Step::VisualReview => "semantic_review",
            _ => return Err(WorkflowError::InvalidRecord),
        };
        let (code, message) = match skipped.get(step) {
            Some(DefaultSkipReason::ModelNotConfigured) => (
                "MODEL_NOT_CONFIGURED",
                "No model is configured for this check.",
            ),
            Some(DefaultSkipReason::CapabilityUnavailable) => (
                "CAPABILITY_UNAVAILABLE",
                "The required capability was unavailable.",
            ),
            Some(DefaultSkipReason::SkippedByAdjustment) => (
                "SKIPPED_BY_ADJUSTMENT",
                "An accepted plan adjustment skipped this check.",
            ),
            Some(DefaultSkipReason::NotSelectedByDefault) => (
                "NOT_SELECTED_BY_DEFAULT",
                "The local default plan did not select this check.",
            ),
            None if plan
                .enabled_steps
                .iter()
                .any(|enabled| enabled.step() == *step) =>
            {
                (
                    "NO_ACCEPTED_EVIDENCE",
                    "No accepted evidence was produced for this planned check.",
                )
            }
            None => (
                "NOT_SELECTED_BY_PLAN",
                "The accepted plan did not select this check.",
            ),
        };
        let mut gap = check(
            name,
            &format_digest(&plan.expected_scope_fingerprint),
            total_assets,
        );
        gap.skip_or_failure_reason = Some(CheckReason {
            code: code.into(),
            message: message.into(),
        });
        checks.push(gap);
    }
    Ok(checks)
}

fn check(name: &str, input: &str, total_assets: u32) -> ReportCheck {
    ReportCheck {
        check_id: name.into(),
        check_contract_version: "1".into(),
        execution_status: "SKIPPED".into(),
        assessment: "NOT_EVALUATED".into(),
        skip_or_failure_reason: None,
        input_fingerprint: input.into(),
        processed_assets: 0,
        total_assets,
        finding_count: 0,
        findings: Vec::new(),
        evidence_refs: Vec::new(),
        started_at: None,
        finished_at: None,
    }
}

fn coverage_reason() -> CheckReason {
    CheckReason {
        code: "INCOMPLETE_COVERAGE".into(),
        message: "Some assets did not produce an evaluated check result.".into(),
    }
}
