//! Resume a paused geometry batch without replaying accepted checks.

use dd_core::workflow::PlanPolicy;
use uuid::Uuid;

use super::geometry::{geometry_assets, prepare_geometry_task};
use super::resume::{PendingTaskBinding, ResumeScope, accepted_profile, activate};
use super::{
    WorkflowError, WorkflowGeometryAcceptance, WorkflowGeometryRequest, WorkflowResumeRequest,
    WorkflowStore, database,
};
use crate::geometry_runtime::read_bound_geometry_report;

impl WorkflowStore {
    pub fn resume_geometry(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: WorkflowGeometryRequest,
        now_ms: i64,
    ) -> Result<WorkflowGeometryAcceptance, WorkflowError> {
        let approved = self.verify_resume(
            session,
            policy,
            &WorkflowResumeRequest {
                reference: request.reference.clone(),
                expected_row_revision: request.expected_row_revision,
            },
        )?;
        self.resume_geometry_batch(session, approved, &request, now_ms)
    }

    pub(super) fn resume_geometry_batch(
        &self,
        session: Uuid,
        approved: ResumeScope,
        request: &WorkflowGeometryRequest,
        now_ms: i64,
    ) -> Result<WorkflowGeometryAcceptance, WorkflowError> {
        let ResumeScope { row, plan, scope } = approved;
        let run = row.run_id;
        let profile = accepted_profile(&self.owner, session, &row)?;
        let assets = geometry_assets(&plan, &scope)?;
        if row.geometry_tasks.is_empty() || row.geometry_tasks.len() != assets.len() {
            return Err(WorkflowError::ControlUnavailable);
        }
        let mut pending = Vec::new();
        for (task, input) in row.geometry_tasks.iter().zip(assets) {
            if input["asset_id"].as_str().and_then(|id| id.parse().ok()) != Some(task.asset_id) {
                return Err(WorkflowError::InputChanged);
            }
            match task.status.as_str() {
                "SUCCEEDED" => {
                    let image = self
                        .owner
                        .registered_image(session, row.dataset_id, task.asset_id)
                        .map_err(WorkflowError::Storage)?;
                    read_bound_geometry_report(&self.owner, session, task.task_id, &image)
                        .map_err(|error| WorkflowError::Execution(Box::new(error)))?;
                }
                "QUEUED" => {
                    let (_, prepared) = prepare_geometry_task(
                        &self.owner,
                        session,
                        row.dataset_id,
                        input,
                        &scope["label_schema_digest"],
                        request,
                    )?;
                    let binding = PendingTaskBinding::new(task.task_id, &prepared.submission)?;
                    pending.push((binding, prepared));
                }
                _ => return Err(WorkflowError::ControlUnavailable),
            }
        }
        let bindings: Vec<_> = pending.iter().map(|(binding, _)| binding.clone()).collect();
        let tasks = self
            .owner
            .workspace_action(session, move |owner| {
                let transaction = owner.connection.transaction().map_err(database)?;
                if activate(&transaction, session, &row, profile, "SUCCEEDED", now_ms)? != 1 {
                    return Ok(Err(WorkflowError::RevisionConflict));
                }
                let mut rebound = Vec::with_capacity(bindings.len());
                for binding in bindings {
                    let Some(task) = binding.rebind(&transaction, session, &row, now_ms)? else {
                        return Ok(Err(WorkflowError::InputChanged));
                    };
                    rebound.push(task);
                }
                transaction.commit().map_err(database)?;
                Ok(Ok(rebound))
            })
            .map_err(WorkflowError::Storage)??;
        let jobs = pending
            .into_iter()
            .zip(tasks)
            .map(|((_, prepared), task)| prepared.into_job(task))
            .collect();
        Ok(WorkflowGeometryAcceptance {
            workflow: self.get(session, run)?,
            jobs,
        })
    }
}
