//! Resume a paused health check from its accepted profile.

use dd_core::workflow::{PlanPolicy, TemplatePlan, validate_plan};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use uuid::Uuid;

use super::{
    PlanRef, RowData, WorkflowError, WorkflowSnapshot, WorkflowStore, database, path_text,
};
use crate::dataset_profile_runtime::read_dataset_profile;
use crate::sqlite::row_uuid;
use crate::task_store::{StoredTask, TaskSubmission, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

#[derive(Clone, Debug)]
pub struct WorkflowResumeRequest {
    pub reference: PlanRef,
    pub expected_row_revision: u64,
}

pub(super) struct ResumeScope {
    pub(super) row: RowData,
    pub(super) plan: TemplatePlan,
    pub(super) scope: Value,
}

impl WorkflowStore {
    pub fn resume(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: WorkflowResumeRequest,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let verified = self.verify_resume(session, policy, &request)?;
        self.resume_from_profile(session, verified.row, now_ms)
    }

    pub(super) fn verify_resume(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: &WorkflowResumeRequest,
    ) -> Result<ResumeScope, WorkflowError> {
        let run = request.reference.run_id;
        let row = self.read_row(session, run)?;
        let stored = row.plan()?;
        if row.revision()? != request.expected_row_revision
            || stored.reference(run)? != request.reference
        {
            return Err(WorkflowError::RevisionConflict);
        }
        if row.status != "PAUSED" || !row.geometry_tasks.is_empty() {
            return Err(WorkflowError::ControlUnavailable);
        }
        let (input, scope) =
            self.verify_input_manifest(session, row.dataset_id, &row.input_path, &stored.input)?;
        let plan = stored
            .plan
            .ok_or(WorkflowError::InvalidRecord)?
            .into_plan()?;
        if plan.template_id != row.template_key {
            return Err(WorkflowError::InvalidRecord);
        }
        validate_plan(&input, policy, &plan).map_err(WorkflowError::Plan)?;
        Ok(ResumeScope { row, plan, scope })
    }

    pub(super) fn resume_from_profile(
        &self,
        session: Uuid,
        row: RowData,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let run = row.run_id;
        let profile = row
            .profile_task
            .as_ref()
            .filter(|task| task.status == "SUCCEEDED")
            .ok_or(WorkflowError::ControlUnavailable)?
            .task_id;
        read_dataset_profile(&self.owner, session, row.dataset_id, profile)
            .map_err(|error| WorkflowError::Execution(Box::new(error)))?;
        let updated = self
            .owner
            .workspace_action(session, move |owner| {
                activate(
                    &owner.connection,
                    session,
                    &row,
                    profile,
                    "SUCCEEDED",
                    now_ms,
                )
            })
            .map_err(WorkflowError::Storage)?;
        if updated != 1 {
            return Err(WorkflowError::RevisionConflict);
        }
        self.get(session, run)
    }

    pub(super) fn resume_queued_profile(
        &self,
        session: Uuid,
        row: RowData,
        submission: &TaskSubmission,
        now_ms: i64,
    ) -> Result<(WorkflowSnapshot, StoredTask), WorkflowError> {
        let run = row.run_id;
        let profile = row
            .profile_task
            .as_ref()
            .filter(|task| task.status == "QUEUED")
            .ok_or(WorkflowError::ControlUnavailable)?
            .task_id;
        let path = path_text(&submission.input_manifest_path)?;
        let fingerprint = format_digest(&submission.input_fingerprint);
        let capability = submission.capability_id.clone();
        let task = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            if activate(&transaction, session, &row, profile, "QUEUED", now_ms)? != 1 {
                return Ok(Err(WorkflowError::RevisionConflict));
            }
            let rebound = transaction.query_row(
                "UPDATE tasks SET workspace_session_id=?3, updated_at_ms=?4, row_revision=row_revision+1
                 WHERE task_id=?1 AND workflow_run_id=?2 AND status='QUEUED'
                   AND attempt_no=0 AND current_attempt_id IS NULL AND dataset_id=?5
                   AND input_manifest_path=?6 AND input_fingerprint=?7 AND capability_id=?8
                 RETURNING request_id,created_at_ms",
                params![profile.to_string(), run.to_string(), session.to_string(), now_ms, row.dataset_id.to_string(), path, fingerprint, capability],
                |record| Ok(StoredTask {
                    task_id: profile, request_id: row_uuid(record, 0)?, workspace_session: session,
                    status: "QUEUED".into(), created_at_ms: record.get(1)?,
                }),
            ).optional().map_err(database)?;
            let Some(rebound) = rebound else { return Ok(Err(WorkflowError::InputChanged)); };
            transaction.commit().map_err(database)?;
            Ok(Ok(rebound))
        }).map_err(WorkflowError::Storage)??;
        Ok((self.get(session, run)?, task))
    }
}

fn activate(
    connection: &Connection,
    session: Uuid,
    row: &RowData,
    profile: Uuid,
    status: &str,
    now_ms: i64,
) -> Result<usize, WorkspaceThreadError> {
    connection
        .execute(
            "UPDATE workflow_runs SET status='RUNNING',workspace_session_id=?3,
            node_states_json=json_set(node_states_json,'$.control.pause_requested',json('false')),
            updated_at_ms=?4,row_revision=row_revision+1
         WHERE workflow_run_id=?1 AND row_revision=?2 AND status='PAUSED'
           AND EXISTS (SELECT 1 FROM tasks WHERE task_id=?5 AND workflow_run_id=?1 AND status=?6)",
            params![
                row.run_id.to_string(),
                row.row_revision,
                session.to_string(),
                now_ms,
                profile.to_string(),
                status
            ],
        )
        .map_err(database)
}
