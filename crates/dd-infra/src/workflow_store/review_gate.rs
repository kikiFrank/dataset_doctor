//! Complete the human gate with current Final and post-review geometry evidence.

mod evidence;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::profile::verified_replay;
use super::report::{KIND, report_path, seal_report_task};
use super::{WorkflowError, WorkflowReport, WorkflowStore, database};
use crate::artifact_reference::ArtifactReference;
use crate::input_fingerprint::freeze_input_manifest;
use crate::paths::publish_immutable;
use crate::sqlite::row_uuid;
use crate::task_store::{TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

pub(super) const CAPABILITY: &str = "workflow_review_complete.v1";

#[derive(Clone, Debug)]
pub struct WorkflowReviewRequest {
    pub workflow_run_id: Uuid,
    pub expected_row_revision: u64,
    pub expected_report: ArtifactReference,
    pub evidence: Vec<WorkflowReviewEvidence>,
    pub acknowledged_incomplete_checks: Vec<WorkflowReviewCheckAcknowledgment>,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkflowReviewEvidence {
    pub asset_id: Uuid,
    pub final_operation_id: Uuid,
    pub geometry_task_id: Uuid,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkflowReviewCheckAcknowledgment {
    pub check_id: String,
    pub reason: String,
}

impl WorkflowStore {
    pub fn complete_review(
        &self,
        session: Uuid,
        mut request: WorkflowReviewRequest,
        now_ms: i64,
    ) -> Result<WorkflowReport, WorkflowError> {
        normalize(&mut request);
        let run = request.workflow_run_id;
        if run.is_nil()
            || request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || now_ms < 0
        {
            return Err(WorkflowError::InvalidInputScope);
        }
        let intent = json!({
            "workflow_run_id": run, "expected_row_revision": request.expected_row_revision.to_string(),
            "source_report": request.expected_report, "review_evidence": request.evidence,
            "acknowledged_incomplete_checks": request.acknowledged_incomplete_checks,
        });
        let intent_bytes = serde_json::to_vec(&intent).map_err(WorkflowError::Json)?;
        let intent_digest: [u8; 32] = Sha256::digest(&intent_bytes).into();
        let key = request.idempotency_key.to_string();
        let fingerprint = format_digest(&intent_digest);

        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(WorkflowError::Storage)?;
        let existing = self
            .owner
            .workspace_action(session, move |owner| completion(&owner.connection, run))
            .map_err(WorkflowError::Storage)?;
        if let Some(existing) = existing {
            let task = replay(existing, &key, &fingerprint)?;
            return self.read_report_by_task(session, run, task);
        }

        let row = self.read_row(session, run)?;
        if row.revision()? != request.expected_row_revision {
            return Err(WorkflowError::RevisionConflict);
        }
        if row.status != "WAITING_REVIEW" {
            return Err(WorkflowError::UpstreamNotReady);
        }
        let original = self.read_report(session, run)?;
        if original.reference != request.expected_report
            || original.data.human_ack_status != "PENDING"
        {
            return Err(WorkflowError::InputChanged);
        }
        let mut node_states: Value =
            serde_json::from_str(&row.node_states_json).map_err(WorkflowError::Json)?;
        let review_node = node_states["nodes"]
            .as_array_mut()
            .and_then(|nodes| {
                nodes
                    .iter_mut()
                    .find(|node| node["node_id"] == "final_annotation")
            })
            .filter(|node| node["status"] == "WAITING_REVIEW")
            .ok_or(WorkflowError::InvalidRecord)?;
        review_node["status"] = json!("SUCCEEDED");
        validate_coverage(&original, &request)?;
        let mut annotations = BTreeMap::new();
        let mut outcomes = Vec::with_capacity(request.evidence.len());
        for entry in &request.evidence {
            if row
                .geometry_tasks
                .iter()
                .any(|task| task.task_id == entry.geometry_task_id)
            {
                return Err(WorkflowError::InputChanged);
            }
            let current = evidence::verify(
                &self.owner,
                session,
                row.dataset_id,
                original.task_id,
                entry,
                now_ms,
            )?;
            annotations.insert(entry.asset_id, current.annotation_digest.clone());
            outcomes.push(serde_json::to_value(current).map_err(WorkflowError::Json)?);
        }
        let plan = row.plan()?;
        let (_, mut scope) = self.verify_current_scope(
            session,
            row.dataset_id,
            &row.input_path,
            &plan.input,
            &annotations,
        )?;
        scope["capability_id"] = json!(CAPABILITY);
        scope["implementation_version"] = json!(env!("CARGO_PKG_VERSION"));
        scope["resolved_config"] = json!({"confirmation": intent, "verified_outcomes": outcomes});
        let frozen = freeze_input_manifest(&self.contracts, scope).map_err(verify_error)?;
        let root = self
            .owner
            .storage_root(session)
            .map_err(WorkflowError::Storage)?;
        let manifest_path = PathBuf::from(format!(
            "artifacts/task-inputs/{}.json",
            format_digest(&frozen.sha256)
        ));
        publish_immutable(&root, &manifest_path, &frozen.bytes).map_err(verify_error)?;
        let submission = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(row.dataset_id),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: KIND.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: manifest_path,
            request_fingerprint: intent_digest,
            input_fingerprint: frozen.input_fingerprint,
        };

        let expected_path = report_path(original.task_id);
        let mut updated = original.data;
        updated.report_id = Uuid::new_v4();
        updated.input_fingerprint = format_digest(&frozen.input_fingerprint);
        updated.human_ack_status = "ACKNOWLEDGED".into();
        updated.repair_outcomes = outcomes;
        let task = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            if let Some(existing) = completion(&transaction, run)? {
                return Ok(replay(existing, &key, &fingerprint));
            }
            let task = enqueue_task_in(&transaction, &submission, now_ms).map_err(WorkspaceThreadError::Task)?;
            let path = match seal_report_task(&transaction, &root, task.task_id, run, &updated, now_ms) {
                Ok(path) => path,
                Err(error) => return Ok(Err(error)),
            };
            node_states["control"]["review_completion_task_id"] = json!(task.task_id);
            let changed = transaction.execute(
                "UPDATE workflow_runs SET status='COMPLETED', result_manifest_path=?4,
                    workspace_session_id=?5, updated_at_ms=?6, finished_at_ms=?6, row_revision=row_revision+1,
                    node_states_json=?7
                 WHERE workflow_run_id=?1 AND row_revision=?2 AND status='WAITING_REVIEW' AND result_manifest_path=?3",
                params![run.to_string(), row.row_revision, expected_path, path, session.to_string(), now_ms, node_states.to_string()],
            ).map_err(database)?;
            if changed != 1 {
                return Ok(Err(WorkflowError::RevisionConflict));
            }
            transaction.commit().map_err(database)?;
            Ok(Ok(task.task_id))
        }).map_err(WorkflowError::Storage)??;
        self.read_report_by_task(session, run, task)
    }
}

fn normalize(request: &mut WorkflowReviewRequest) {
    request
        .evidence
        .sort_unstable_by_key(|entry| entry.asset_id);
    for check in &mut request.acknowledged_incomplete_checks {
        check.reason = check.reason.trim().to_owned();
    }
    request
        .acknowledged_incomplete_checks
        .sort_unstable_by(|a, b| a.check_id.cmp(&b.check_id));
}

fn validate_coverage(
    report: &WorkflowReport,
    request: &WorkflowReviewRequest,
) -> Result<(), WorkflowError> {
    let findings: Vec<_> = report
        .data
        .checks
        .iter()
        .flat_map(|check| &check.findings)
        .filter(|finding| finding.severity != "INFO")
        .collect();
    if findings
        .iter()
        .any(|finding| finding.check_id != "annotation_geometry")
    {
        return Err(WorkflowError::UpstreamNotReady);
    }
    let required: BTreeSet<_> = findings
        .into_iter()
        .map(|finding| finding.asset_id)
        .collect();
    if !required
        .into_iter()
        .eq(request.evidence.iter().map(|entry| entry.asset_id))
    {
        return Err(WorkflowError::InvalidInputScope);
    }
    let incomplete: BTreeSet<_> = report
        .data
        .unexecuted_checks
        .iter()
        .map(String::as_str)
        .collect();
    if request
        .acknowledged_incomplete_checks
        .iter()
        .any(|entry| entry.reason.is_empty())
        || !incomplete.into_iter().eq(request
            .acknowledged_incomplete_checks
            .iter()
            .map(|entry| entry.check_id.as_str()))
    {
        return Err(WorkflowError::InvalidInputScope);
    }
    Ok(())
}

fn completion(
    connection: &Connection,
    run: Uuid,
) -> Result<Option<(Uuid, String, String)>, WorkspaceThreadError> {
    connection.query_row(
        "SELECT task_id,idempotency_key,request_fingerprint FROM tasks
         WHERE workflow_run_id=?1 AND operation_kind=?2 AND capability_id=?3 AND status='SUCCEEDED'",
        params![run.to_string(), KIND, CAPABILITY],
        |row| Ok((row_uuid(row, 0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(database)
}

fn replay(
    existing: (Uuid, String, String),
    key: &str,
    fingerprint: &str,
) -> Result<Uuid, WorkflowError> {
    let (task, previous_key, previous_fingerprint) = existing;
    verified_replay(Some((previous_key, previous_fingerprint)), key, fingerprint)?;
    Ok(task)
}

fn verify_error(error: impl std::error::Error + Send + Sync + 'static) -> WorkflowError {
    WorkflowError::Execution(Box::new(error))
}
