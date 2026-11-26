//! Bind the first health-check node to a durable Worker task.

use dd_core::workflow::{EnabledStep, PlanPolicy, validate_plan};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{PROFILE_NODE, PlanRef, WorkflowError, WorkflowSnapshot, WorkflowStore, database};
use crate::dataset_profile_runtime::{
    DatasetProfileJob, DatasetProfileRequest, DatasetProfileRuntime,
};
use crate::task_store::{enqueue_task_in, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

#[derive(Clone, Debug)]
pub struct WorkflowProfileRequest {
    pub reference: PlanRef,
    pub expected_row_revision: u64,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub max_pixels: u32,
}

pub struct WorkflowProfileAcceptance {
    pub workflow: WorkflowSnapshot,
    pub job: Option<DatasetProfileJob>,
}

impl WorkflowStore {
    pub fn start_profile(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: WorkflowProfileRequest,
        now_ms: i64,
    ) -> Result<WorkflowProfileAcceptance, WorkflowError> {
        let run = request.reference.run_id;
        let row = self.read_row(session, run)?;
        let stored = row.plan()?;
        if stored.reference(run)? != request.reference {
            return Err(WorkflowError::RevisionConflict);
        }
        let fingerprint = command_fingerprint(&request);
        let first_key = request.idempotency_key.to_string();
        let expected_fingerprint = format_digest(&fingerprint);
        if row.status != "READY" {
            self.owner
                .workspace_action(session, move |owner| {
                    let existing = profile_command(&owner.connection, run)?;
                    Ok(verified_replay(existing, &first_key, &expected_fingerprint))
                })
                .map_err(WorkflowError::Storage)??;
            return Ok(WorkflowProfileAcceptance {
                workflow: self.get(session, run)?,
                job: None,
            });
        }
        if row.revision()? != request.expected_row_revision {
            return Err(WorkflowError::RevisionConflict);
        }
        let (source, frozen_scope) =
            self.verify_input_manifest(session, row.dataset_id, &row.input_path, &stored.input)?;
        let plan = stored
            .plan
            .ok_or(WorkflowError::InvalidRecord)?
            .into_plan()?;
        if plan.template_id != row.template_key {
            return Err(WorkflowError::InvalidRecord);
        }
        validate_plan(&source, policy, &plan).map_err(WorkflowError::Plan)?;
        let limit = plan
            .enabled_steps
            .iter()
            .find_map(|step| match step {
                EnabledStep::DatasetProfile { sample_limit } => Some(*sample_limit as usize),
                _ => None,
            })
            .ok_or(WorkflowError::InvalidInputScope)?;
        let expected: Vec<_> = frozen_scope["assets"]
            .as_array()
            .ok_or(WorkflowError::InvalidRecord)?
            .iter()
            .take(limit)
            .cloned()
            .collect();
        let asset_ids = expected
            .iter()
            .map(|entry| {
                entry["asset_id"]
                    .as_str()
                    .and_then(|id| id.parse().ok())
                    .ok_or(WorkflowError::InvalidRecord)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut prepared = DatasetProfileRuntime::prepare(
            &self.owner,
            session,
            DatasetProfileRequest {
                dataset_id: row.dataset_id,
                asset_ids,
                max_pixels: request.max_pixels,
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
            },
        )
        .map_err(|error| WorkflowError::Execution(Box::new(error)))?;
        let projected: serde_json::Value =
            serde_json::from_slice(prepared.manifest_bytes()).map_err(WorkflowError::Json)?;
        if projected["assets"].as_array() != Some(&expected)
            || projected["label_schema_digest"] != frozen_scope["label_schema_digest"]
        {
            return Err(WorkflowError::InputChanged);
        }
        prepared.submission.request_fingerprint = fingerprint;
        let submission = prepared.submission.clone();
        let admitted = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            if let Some(existing) = profile_command(&transaction, run)? {
                return Ok(verified_replay(Some(existing), &first_key, &expected_fingerprint).map(|()| None));
            }
            let task = enqueue_task_in(&transaction, &submission, now_ms).map_err(WorkspaceThreadError::Task)?;
            let attached = transaction.execute(
                "UPDATE tasks SET workflow_run_id=?1,node_id=?2 WHERE task_id=?3 AND status='QUEUED' AND workflow_run_id IS NULL",
                params![run.to_string(), PROFILE_NODE, task.task_id.to_string()],
            ).map_err(database)?;
            if attached != 1 {
                return Ok(Err(WorkflowError::RevisionConflict));
            }
            let nodes = json!({
                "nodes": [{"node_id": PROFILE_NODE, "stage_id":"preflight", "task_id":task.task_id}],
                "control": {"pause_requested":false},
            }).to_string();
            let activated = transaction.execute(
                "UPDATE workflow_runs SET status='RUNNING',workspace_session_id=?3,node_states_json=?4,
                    started_at_ms=?5,updated_at_ms=?5,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND status='READY' AND row_revision=?2",
                params![run.to_string(), row.row_revision, session.to_string(), nodes, now_ms],
            ).map_err(database)?;
            if activated != 1 {
                return Ok(Err(WorkflowError::RevisionConflict));
            }
            transaction.commit().map_err(database)?;
            Ok(Ok(Some(task)))
        }).map_err(WorkflowError::Storage)??;
        Ok(WorkflowProfileAcceptance {
            workflow: self.get(session, run)?,
            job: admitted.map(|task| prepared.into_job(task)),
        })
    }
}

fn command_fingerprint(request: &WorkflowProfileRequest) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"datasetdoctor.workflow.start-profile.v1\0");
    digest.update(request.reference.run_id.as_bytes());
    digest.update(request.reference.plan_revision.to_le_bytes());
    digest.update(request.reference.input_fingerprint);
    digest.update(request.expected_row_revision.to_le_bytes());
    digest.update(request.max_pixels.to_le_bytes());
    digest.finalize().into()
}

fn profile_command(
    connection: &Connection,
    run: Uuid,
) -> Result<Option<(String, String)>, WorkspaceThreadError> {
    connection.query_row(
        "SELECT idempotency_key,request_fingerprint FROM tasks WHERE workflow_run_id=?1 AND node_id=?2",
        params![run.to_string(), PROFILE_NODE], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(database)
}

fn verified_replay(
    existing: Option<(String, String)>,
    key: &str,
    fingerprint: &str,
) -> Result<(), WorkflowError> {
    match existing {
        Some((old_key, old_fp)) if old_key == key && old_fp == fingerprint => Ok(()),
        Some((old_key, _)) if old_key == key => Err(WorkflowError::IdempotencyConflict),
        _ => Err(WorkflowError::RevisionConflict),
    }
}
