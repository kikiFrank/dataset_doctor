//! Admit the planned geometry checks after the upstream profile is verified.

use dd_core::workflow::{EnabledStep, PlanPolicy, validate_plan};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{GEOMETRY_NODE, WorkflowError, WorkflowSnapshot, WorkflowStore, database};
use crate::dataset_profile_runtime::read_dataset_profile;
use crate::geometry_runtime::{GeometryAuditJob, GeometryAuditRequest, GeometryRuntime};
use crate::task_store::{enqueue_task_in, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

pub type WorkflowGeometryRequest = super::WorkflowProfileRequest;

pub struct WorkflowGeometryAcceptance {
    pub workflow: WorkflowSnapshot,
    pub jobs: Vec<GeometryAuditJob>,
}

impl WorkflowStore {
    pub fn start_geometry(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: WorkflowGeometryRequest,
        now_ms: i64,
    ) -> Result<WorkflowGeometryAcceptance, WorkflowError> {
        let run = request.reference.run_id;
        let row = self.read_row(session, run)?;
        let stored = row.plan()?;
        if stored.reference(run)? != request.reference {
            return Err(WorkflowError::RevisionConflict);
        }
        let key = request.idempotency_key.to_string();
        let fingerprint = format_digest(&super::profile::command_fingerprint(
            &request,
            b"datasetdoctor.workflow.start-geometry.v1\0",
        ));
        if is_replay(&row.node_states_json, &key, &fingerprint)? {
            return Ok(WorkflowGeometryAcceptance {
                workflow: self.get(session, run)?,
                jobs: Vec::new(),
            });
        }
        if super::control::pause_requested(&row.status, &row.node_states_json)? {
            return Err(WorkflowError::Paused);
        }
        if row.status != "RUNNING"
            || row.session != session
            || row.revision()? != request.expected_row_revision
        {
            return Err(WorkflowError::RevisionConflict);
        }
        let profile = row
            .profile_task
            .as_ref()
            .filter(|task| task.status == "SUCCEEDED")
            .ok_or(WorkflowError::UpstreamNotReady)?;
        read_dataset_profile(&self.owner, session, row.dataset_id, profile.task_id)
            .map_err(|error| WorkflowError::Execution(Box::new(error)))?;
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
                EnabledStep::AnnotationGeometry { sample_limit } => Some(*sample_limit as usize),
                _ => None,
            })
            .ok_or(WorkflowError::InvalidInputScope)?;
        let mut prepared = Vec::new();
        for expected in frozen_scope["assets"]
            .as_array()
            .ok_or(WorkflowError::InvalidRecord)?
            .iter()
            .take(limit)
        {
            let asset: Uuid = expected["asset_id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .ok_or(WorkflowError::InvalidRecord)?;
            let job = GeometryRuntime::prepare(
                &self.owner,
                session,
                GeometryAuditRequest {
                    dataset_id: row.dataset_id,
                    asset_id: asset,
                    request_id: request.request_id,
                    idempotency_key: Uuid::new_v4(),
                    max_pixels: u64::from(request.max_pixels),
                },
            )
            .map_err(|error| WorkflowError::Execution(Box::new(error)))?;
            let projected: Value =
                serde_json::from_slice(job.manifest_bytes()).map_err(WorkflowError::Json)?;
            if projected["assets"][0] != *expected
                || projected["label_schema_digest"] != frozen_scope["label_schema_digest"]
            {
                return Err(WorkflowError::InputChanged);
            }
            prepared.push((asset, job));
        }
        let submissions: Vec<_> = prepared
            .iter()
            .map(|(asset, job)| (*asset, job.submission.clone()))
            .collect();
        let queued = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            let current = transaction.query_row(
                "SELECT node_states_json FROM workflow_runs WHERE workflow_run_id=?1 AND status='RUNNING' AND workspace_session_id=?2",
                params![run.to_string(),session.to_string()], |row| row.get::<_, String>(0),
            ).optional().map_err(database)?;
            let Some(current) = current else { return Ok(Err(WorkflowError::RevisionConflict)); };
            match is_replay(&current, &key, &fingerprint) {
                Ok(true) => return Ok(Ok(None)),
                Ok(false) => (),
                Err(error) => return Ok(Err(error)),
            }
            let mut nodes: Value = match serde_json::from_str(&current) {
                Ok(nodes) => nodes,
                Err(error) => return Ok(Err(WorkflowError::Json(error))),
            };
            let Some(bindings) = nodes["nodes"].as_array_mut() else { return Ok(Err(WorkflowError::InvalidRecord)); };
            let mut media = Vec::with_capacity(submissions.len());
            let mut tasks = Vec::with_capacity(submissions.len());
            for (asset, submission) in submissions {
                let task = enqueue_task_in(&transaction, &submission, now_ms).map_err(WorkspaceThreadError::Task)?;
                let attached = transaction.execute(
                    "UPDATE tasks SET workflow_run_id=?1,node_id=?2 WHERE task_id=?3 AND status='QUEUED' AND workflow_run_id IS NULL",
                    params![run.to_string(), format!("{GEOMETRY_NODE}/{asset}"), task.task_id.to_string()],
                ).map_err(database)?;
                if attached != 1 {
                    return Ok(Err(WorkflowError::RevisionConflict));
                }
                media.push(json!({"asset_id":asset,"task_id":task.task_id}));
                tasks.push(task);
            }
            bindings.push(json!({"node_id":GEOMETRY_NODE,"stage_id":"checks","idempotency_key":key,
                "request_fingerprint":fingerprint,"tasks":media}));
            let changed = transaction.execute(
                "UPDATE workflow_runs SET node_states_json=?4,updated_at_ms=?5,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND status='RUNNING' AND row_revision=?2 AND workspace_session_id=?3",
                params![run.to_string(),row.row_revision,session.to_string(),nodes.to_string(),now_ms],
            ).map_err(database)?;
            if changed != 1 {
                return Ok(Err(WorkflowError::RevisionConflict));
            }
            transaction.commit().map_err(database)?;
            Ok(Ok(Some(tasks)))
        }).map_err(WorkflowError::Storage)??;
        let jobs = queued
            .map(|tasks| {
                prepared
                    .into_iter()
                    .zip(tasks)
                    .map(|((_, prepared), task)| prepared.into_job(task))
                    .collect()
            })
            .unwrap_or_default();
        Ok(WorkflowGeometryAcceptance {
            workflow: self.get(session, run)?,
            jobs,
        })
    }
}

fn is_replay(node_states: &str, key: &str, fingerprint: &str) -> Result<bool, WorkflowError> {
    let states: Value = serde_json::from_str(node_states).map_err(WorkflowError::Json)?;
    let admitted = states["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["node_id"] == GEOMETRY_NODE));
    match admitted {
        None => Ok(false),
        Some(node) if node["idempotency_key"] != key => Err(WorkflowError::RevisionConflict),
        Some(node) if node["request_fingerprint"] != fingerprint => {
            Err(WorkflowError::IdempotencyConflict)
        }
        Some(_) => Ok(true),
    }
}
