//! Resume a paused health check from its accepted profile.

use dd_core::workflow::{PlanPolicy, validate_plan};
use rusqlite::params;
use uuid::Uuid;

use super::{PlanRef, WorkflowError, WorkflowSnapshot, WorkflowStore, database};
use crate::dataset_profile_runtime::read_dataset_profile;

#[derive(Clone, Debug)]
pub struct WorkflowResumeRequest {
    pub reference: PlanRef,
    pub expected_row_revision: u64,
}

impl WorkflowStore {
    pub fn resume(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: WorkflowResumeRequest,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
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
        let profile = row
            .profile_task
            .as_ref()
            .filter(|task| task.status == "SUCCEEDED")
            .ok_or(WorkflowError::ControlUnavailable)?;
        let input = self.verify_input(session, row.dataset_id, &row.input_path, &stored.input)?;
        let plan = stored
            .plan
            .ok_or(WorkflowError::InvalidRecord)?
            .into_plan()?;
        if plan.template_id != row.template_key {
            return Err(WorkflowError::InvalidRecord);
        }
        validate_plan(&input, policy, &plan).map_err(WorkflowError::Plan)?;
        read_dataset_profile(&self.owner, session, row.dataset_id, profile.task_id)
            .map_err(|error| WorkflowError::Execution(Box::new(error)))?;
        let profile = profile.task_id;
        let updated = self.owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "UPDATE workflow_runs SET status='RUNNING',workspace_session_id=?3,
                    node_states_json=json_set(node_states_json,'$.control.pause_requested',json('false')),
                    updated_at_ms=?4,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND row_revision=?2 AND status='PAUSED'
                   AND EXISTS (SELECT 1 FROM tasks WHERE task_id=?5 AND workflow_run_id=?1 AND status='SUCCEEDED')",
                params![run.to_string(), row.row_revision, session.to_string(), now_ms, profile.to_string()],
            ).map_err(database)
        }).map_err(WorkflowError::Storage)?;
        if updated != 1 {
            return Err(WorkflowError::RevisionConflict);
        }
        self.get(session, run)
    }
}
