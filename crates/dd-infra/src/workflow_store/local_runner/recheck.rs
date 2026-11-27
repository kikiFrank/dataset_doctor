//! Persist the reviewed inputs before dispatching their real geometry rechecks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::super::resume::PendingTaskBinding;
use super::super::review_gate::{evidence, normalize, validate_coverage};
use super::super::{
    WorkflowError, WorkflowReport, WorkflowReviewCheckAcknowledgment, WorkflowReviewEvidence,
    WorkflowReviewRequest, WorkflowStore, database,
};
use super::{LocalHealthcheckRuntime, execution};
use crate::artifact_reference::ArtifactReference;
use crate::geometry_runtime::{
    GeometryAuditJob, GeometryAuditRequest, GeometryRuntime, GeometryRuntimeError,
};
use crate::input_fingerprint::manifest_u32;
use crate::task_store::{TaskStoreError, enqueue_task_in, format_digest};
use crate::worker_runtime::now_ms;
use crate::workspace_thread::WorkspaceThreadError;

#[derive(Clone, Debug)]
pub struct WorkflowRecheckRequest {
    pub workflow_run_id: Uuid,
    pub expected_row_revision: u64,
    pub expected_report: ArtifactReference,
    pub acknowledged_incomplete_checks: Vec<WorkflowReviewCheckAcknowledgment>,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

pub struct WorkflowRecheckJob {
    session: Uuid,
    review: WorkflowReviewRequest,
    jobs: Vec<GeometryAuditJob>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    idempotency_key: Uuid,
    fingerprint: String,
    review_revision: u64,
    evidence: Vec<WorkflowReviewEvidence>,
}

impl LocalHealthcheckRuntime {
    pub fn prepare_review_completion(
        &self,
        session: Uuid,
        request: WorkflowRecheckRequest,
    ) -> Result<WorkflowRecheckJob, WorkflowError> {
        self.store.prepare_recheck(session, request)
    }

    pub async fn complete_after_review(
        &self,
        session: Uuid,
        request: WorkflowRecheckRequest,
    ) -> Result<WorkflowReport, WorkflowError> {
        let store = Arc::clone(&self.store);
        let job = tokio::task::spawn_blocking(move || store.prepare_recheck(session, request))
            .await
            .map_err(execution)??;
        self.run_review_completion(job).await
    }

    pub async fn run_review_completion(
        &self,
        job: WorkflowRecheckJob,
    ) -> Result<WorkflowReport, WorkflowError> {
        let session = job.session;
        let run = job.review.workflow_run_id;
        let revision = i64::try_from(job.review.expected_row_revision)
            .map_err(|_| WorkflowError::InvalidRecord)?;
        let result = self.execute_review_completion(job).await;
        if let Err(error) = &result {
            let code = if matches!(error, WorkflowError::InputChanged) {
                "INPUT_CHANGED"
            } else {
                "REVIEW_RECHECK_FAILED"
            };
            if let Ok(now) = now_ms() {
                let owner = Arc::clone(&self.store.owner);
                let _ = tokio::task::spawn_blocking(move || owner.workspace_action(session, move |owner| {
                    owner.connection.execute(
                        "UPDATE workflow_runs SET error_code=?3,updated_at_ms=?4,row_revision=row_revision+1
                         WHERE workflow_run_id=?1 AND row_revision=?2 AND status='WAITING_REVIEW'
                           AND NOT EXISTS(SELECT 1 FROM tasks WHERE workflow_run_id=?1
                             AND node_id GLOB 'final_annotation/recheck/*' AND status IN ('DISPATCHING','RUNNING'))",
                        params![run.to_string(), revision, code, now],
                    ).map_err(database)?;
                    Ok(())
                })).await;
            }
        }
        result
    }

    async fn execute_review_completion(
        &self,
        job: WorkflowRecheckJob,
    ) -> Result<WorkflowReport, WorkflowError> {
        let _dispatch = self.review_dispatch.lock().await;
        let session = job.session;
        let owner = Arc::clone(&self.store.owner);
        let jobs = tokio::task::spawn_blocking(move || {
            let mut pending = Vec::new();
            for candidate in job.jobs {
                let outcome = owner
                    .task_outcome(session, candidate.task().task_id)
                    .map_err(WorkflowError::Storage)?;
                match outcome.status.as_str() {
                    "SUCCEEDED" => (),
                    "QUEUED" => pending.push(candidate),
                    _ => return Err(WorkflowError::UpstreamNotReady),
                }
            }
            Ok(pending)
        })
        .await
        .map_err(execution)??;
        if !jobs.is_empty() {
            let mut worker = GeometryRuntime::launch_with_program(
                Arc::clone(&self.store.owner),
                session,
                self.program.clone(),
            )
            .await
            .map_err(execution)?;
            for pending in jobs {
                let task = pending.task().task_id;
                match worker.audit_admitted(pending).await {
                    Ok(outcome) if outcome.status == "SUCCEEDED" => (),
                    Ok(_) => {
                        worker.shutdown().await.map_err(execution)?;
                        return Err(WorkflowError::UpstreamNotReady);
                    }
                    Err(error) => {
                        worker.terminate().await.map_err(execution)?;
                        // Another dispatcher can have won the conditional admission.
                        // Its running attempt must retain ownership of the task.
                        if matches!(
                            &error,
                            GeometryRuntimeError::Workspace(WorkspaceThreadError::Task(
                                TaskStoreError::AttemptConflict
                            ))
                        ) {
                            return Err(execution(error));
                        }
                        GeometryRuntime::record_unavailable(
                            &self.store.owner,
                            session,
                            task,
                            false,
                        )
                        .map_err(execution)?;
                        return Err(execution(error));
                    }
                }
            }
            worker.shutdown().await.map_err(execution)?;
        }
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            store.complete_review(session, job.review, now_ms().map_err(execution)?)
        })
        .await
        .map_err(execution)?
    }
}

impl WorkflowStore {
    fn prepare_recheck(
        &self,
        session: Uuid,
        request: WorkflowRecheckRequest,
    ) -> Result<WorkflowRecheckJob, WorkflowError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(WorkflowError::Storage)?;
        let run = request.workflow_run_id;
        let mut review = WorkflowReviewRequest {
            workflow_run_id: run,
            expected_row_revision: request.expected_row_revision,
            expected_report: request.expected_report,
            evidence: Vec::new(),
            acknowledged_incomplete_checks: request.acknowledged_incomplete_checks,
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
        };
        normalize(&mut review);
        if run.is_nil() || review.request_id.is_nil() || review.idempotency_key.is_nil() {
            return Err(WorkflowError::InvalidInputScope);
        }
        let fingerprint = format_digest(
            &Sha256::digest(
                serde_json::to_vec(&json!({
                    "run":run, "revision":request.expected_row_revision.to_string(),
                    "report":review.expected_report, "key":review.idempotency_key,
                    "acknowledged_incomplete_checks":review.acknowledged_incomplete_checks,
                }))
                .map_err(WorkflowError::Json)?,
            )
            .into(),
        );
        let row = self.read_row(session, run)?;
        let mut states: Value =
            serde_json::from_str(&row.node_states_json).map_err(WorkflowError::Json)?;
        let prior = states["control"]["review_recheck"].clone();
        let mut existing: Option<Batch> = if prior.is_null() {
            None
        } else {
            Some(serde_json::from_value(prior.clone()).map_err(WorkflowError::Json)?)
        };
        if let Some(batch) = &existing
            && batch.fingerprint != fingerprint
        {
            if batch.idempotency_key == review.idempotency_key {
                return Err(WorkflowError::IdempotencyConflict);
            }
            if row.status != "WAITING_REVIEW" || row.revision()? != request.expected_row_revision {
                return Err(WorkflowError::RevisionConflict);
            }
            for entry in &batch.evidence {
                let task = self
                    .owner
                    .task_outcome(session, entry.geometry_task_id)
                    .map_err(WorkflowError::Storage)?;
                if !matches!(
                    task.status.as_str(),
                    "SUCCEEDED" | "FAILED" | "INTERRUPTED" | "CANCELLED"
                ) {
                    return Err(WorkflowError::UpstreamNotReady);
                }
            }
            let history = &mut states["control"]["review_recheck_history"];
            if history.is_null() {
                *history = json!([]);
            }
            let history = history.as_array_mut().ok_or(WorkflowError::InvalidRecord)?;
            if history
                .iter()
                .any(|batch| batch["idempotency_key"] == review.idempotency_key.to_string())
            {
                return Err(WorkflowError::IdempotencyConflict);
            }
            history.push(prior);
            existing = None;
        }
        if let Some(batch) = &existing {
            if batch.fingerprint != fingerprint {
                return Err(WorkflowError::IdempotencyConflict);
            }
            review.expected_row_revision = batch.review_revision;
            review.evidence = batch.evidence.clone();
            if row.status == "COMPLETED" {
                return Ok(WorkflowRecheckJob {
                    session,
                    review,
                    jobs: Vec::new(),
                });
            }
            if row.revision()? != batch.review_revision {
                return Err(WorkflowError::RevisionConflict);
            }
        } else if row.revision()? != request.expected_row_revision {
            return Err(WorkflowError::RevisionConflict);
        }
        if row.status != "WAITING_REVIEW" {
            return Err(WorkflowError::UpstreamNotReady);
        }
        let original = self.read_report(session, run)?;
        if original.reference != review.expected_report {
            return Err(WorkflowError::InputChanged);
        }
        let required: BTreeSet<_> = original
            .data
            .checks
            .iter()
            .flat_map(|check| &check.findings)
            .filter(|finding| finding.severity != "INFO")
            .map(|finding| finding.asset_id)
            .collect();
        let mut annotations = BTreeMap::new();
        let mut current = Vec::new();
        for asset in required {
            let (entry, digest) = evidence::current_final(
                &self.owner,
                session,
                row.dataset_id,
                original.task_id,
                asset,
            )?;
            annotations.insert(asset, digest);
            current.push(entry);
        }
        if existing.is_some()
            && (review.evidence.len() != current.len()
                || review.evidence.iter().zip(&current).any(|(old, new)| {
                    old.asset_id != new.asset_id || old.final_operation_id != new.final_operation_id
                }))
        {
            return Err(WorkflowError::InputChanged);
        }
        if existing.is_none() {
            review.evidence = current;
        }
        validate_coverage(&original, &review)?;
        let (_, scope) = self.verify_current_scope(
            session,
            row.dataset_id,
            &row.input_path,
            &row.plan()?.input,
            &annotations,
        )?;
        let max_pixels = scope["resolved_config"]["max_pixels"]
            .as_number()
            .and_then(manifest_u32)
            .filter(|value| *value > 0)
            .ok_or(WorkflowError::InvalidRecord)?;
        let mut prepared = Vec::new();
        for entry in &review.evidence {
            if existing.is_some() {
                let task = self
                    .owner
                    .task_outcome(session, entry.geometry_task_id)
                    .map_err(WorkflowError::Storage)?;
                if task.status == "SUCCEEDED" {
                    evidence::verify(
                        &self.owner,
                        session,
                        row.dataset_id,
                        original.task_id,
                        entry,
                        now_ms().map_err(execution)?,
                    )?;
                    continue;
                }
                if matches!(task.status.as_str(), "DISPATCHING" | "RUNNING") {
                    continue;
                }
                if task.status != "QUEUED" {
                    return Err(WorkflowError::UpstreamNotReady);
                }
            }
            let job = GeometryRuntime::prepare(
                &self.owner,
                session,
                GeometryAuditRequest {
                    dataset_id: row.dataset_id,
                    asset_id: entry.asset_id,
                    max_pixels: u64::from(max_pixels),
                    request_id: review.request_id,
                    idempotency_key: Uuid::new_v4(),
                },
            )
            .map_err(execution)?;
            prepared.push((entry.asset_id, entry.geometry_task_id, job));
        }
        let now = now_ms().map_err(execution)?;
        let submissions: Vec<_> = prepared
            .iter()
            .map(|(asset, task, job)| (*asset, *task, job.submission.clone()))
            .collect();
        let mut entries = review.evidence.clone();
        let new_batch = existing.is_none();
        let idempotency_key = review.idempotency_key;
        let revision = row
            .revision()?
            .checked_add(u64::from(new_batch))
            .ok_or(WorkflowError::InvalidRecord)?;
        let sql_revision = i64::try_from(revision).map_err(|_| WorkflowError::InvalidRecord)?;
        let tasks = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            let mut tasks = Vec::new();
            for (asset, existing_task, submission) in submissions {
                let task = if new_batch {
                    let task = enqueue_task_in(&transaction, &submission, now).map_err(WorkspaceThreadError::Task)?;
                    let attached = transaction.execute(
                        "UPDATE tasks SET workflow_run_id=?1,node_id=?2 WHERE task_id=?3 AND workflow_run_id IS NULL",
                        params![run.to_string(), format!("final_annotation/recheck/{asset}"), task.task_id.to_string()],
                    ).map_err(database)?;
                    if attached != 1 {
                        return Ok(Err(WorkflowError::RevisionConflict));
                    }
                    let Some(entry) = entries.iter_mut().find(|entry| entry.asset_id == asset) else {
                        return Ok(Err(WorkflowError::InvalidRecord));
                    };
                    entry.geometry_task_id = task.task_id;
                    task
                } else {
                    let binding = PendingTaskBinding::new(existing_task, &submission).map_err(|_| WorkspaceThreadError::Task(crate::task_store::TaskStoreError::AttemptConflict))?;
                    let Some(task) = binding.rebind(&transaction, session, &row, now)? else {
                        return Ok(Err(WorkflowError::InputChanged));
                    };
                    task
                };
                tasks.push(task);
            }
            if new_batch {
                states["control"]["review_recheck"] = serde_json::to_value(Batch {
                    idempotency_key, fingerprint, review_revision: revision, evidence: entries,
                }).map_err(|error| WorkspaceThreadError::Task(crate::task_store::TaskStoreError::Receipt(error)))?;
            }
            let changed = transaction.execute(
                "UPDATE workflow_runs SET node_states_json=?3,workspace_session_id=?4,error_code=NULL,
                    updated_at_ms=?5,row_revision=?6
                 WHERE workflow_run_id=?1 AND row_revision=?2 AND status='WAITING_REVIEW'",
                params![run.to_string(), row.row_revision, states.to_string(), session.to_string(), now, sql_revision],
            ).map_err(database)?;
            if changed != 1 { return Ok(Err(WorkflowError::RevisionConflict)); }
            transaction.commit().map_err(database)?;
            Ok(Ok(tasks))
        }).map_err(WorkflowError::Storage)??;
        if new_batch {
            for (entry, task) in review.evidence.iter_mut().zip(&tasks) {
                entry.geometry_task_id = task.task_id;
            }
        }
        review.expected_row_revision = revision;
        Ok(WorkflowRecheckJob {
            session,
            review,
            jobs: prepared
                .into_iter()
                .zip(tasks)
                .map(|((_, _, job), task)| job.into_job(task))
                .collect(),
        })
    }
}
