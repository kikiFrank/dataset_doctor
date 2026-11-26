//! Save structural adjustments before dispatch, under the accepted plan and row revisions.

use std::collections::BTreeSet;

use dd_core::workflow::{
    AcceptedPlan, AdjustmentAction, AdjustmentEffect, DefaultSkipReason, PlanAdjustment, PlanPolicy,
};
use rusqlite::params;
use uuid::Uuid;

use super::{PlanBody, WorkflowError, WorkflowSnapshot, WorkflowStore, database};

impl WorkflowStore {
    pub fn adjust_ready(
        &self,
        session: Uuid,
        run: Uuid,
        expected_row_revision: u64,
        policy: &PlanPolicy,
        proposal: PlanAdjustment,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let row = self.read_row(session, run)?;
        if row.status != "READY" || row.revision()? != expected_row_revision {
            return Err(WorkflowError::RevisionConflict);
        }
        let mut stored = row.plan()?;
        let input = self.verify_input(session, row.dataset_id, &row.input_path, &stored.input)?;
        let plan = stored
            .plan
            .take()
            .ok_or(WorkflowError::InvalidRecord)?
            .into_plan()?;
        if plan.template_id != row.template_key {
            return Err(WorkflowError::InvalidRecord);
        }
        let mut accepted = AcceptedPlan::restore(
            &input,
            policy,
            plan,
            stored.plan_revision,
            std::mem::take(&mut stored.adjustments),
        )
        .map_err(WorkflowError::Adjustment)?;
        let action = proposal.action;
        let effect = accepted
            .adjust(&input, policy, &BTreeSet::new(), proposal)
            .map_err(WorkflowError::Adjustment)?;
        if effect != AdjustmentEffect::PlanChanged {
            return Err(WorkflowError::ExecutionControlRequired);
        }
        match action {
            AdjustmentAction::EnableCheck(check) => {
                stored.not_executed.remove(&check.step());
            }
            AdjustmentAction::SkipWithReason { step } => {
                stored
                    .not_executed
                    .insert(step, DefaultSkipReason::SkippedByAdjustment);
            }
            _ => (),
        }
        stored.plan_revision = accepted.plan_revision();
        stored.adjustment_count = accepted.adjustment_count();
        stored.adjustments = accepted.history().to_vec();
        stored.plan = Some(PlanBody::from(accepted.plan().clone()));
        let encoded = serde_json::to_string(&stored).map_err(WorkflowError::Json)?;
        let changed = self
            .owner
            .workspace_action(session, move |owner| {
                owner.connection.execute(
                "UPDATE workflow_runs SET plan_json=?3,updated_at_ms=?4,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND row_revision=?2 AND status='READY'",
                params![run.to_string(), row.row_revision, encoded, now_ms],
            ).map_err(database)
            })
            .map_err(WorkflowError::Storage)?;
        if changed != 1 {
            return Err(WorkflowError::RevisionConflict);
        }
        self.get(session, run)
    }
}
