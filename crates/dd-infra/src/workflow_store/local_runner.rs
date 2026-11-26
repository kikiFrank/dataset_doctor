//! Run the registered local health check from a Workspace Dataset.

mod input;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use dd_core::workflow::{PlanPolicy, Step};
use dd_domain::workflow_templates::DATASET_HEALTHCHECK;
use rusqlite::params;
use uuid::Uuid;

use super::{
    PlanRef, WorkflowError, WorkflowGeometryRequest, WorkflowProfileRequest, WorkflowReport,
    WorkflowReportRequest, WorkflowSnapshot, WorkflowStatus, WorkflowStore, database,
};
use crate::dataset_profile_runtime::{DatasetProfileJob, DatasetProfileRuntime};
use crate::geometry_runtime::GeometryRuntime;
use crate::worker_runtime::now_ms;
use crate::workspace_thread::WorkspaceThread;

#[derive(Clone, Debug)]
pub struct LocalHealthcheckRequest {
    pub dataset_id: Uuid,
    pub sample_limit: u32,
    pub max_pixels: u32,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

pub struct LocalHealthcheckAcceptance {
    pub workflow: WorkflowSnapshot,
    pub job: Option<LocalHealthcheckJob>,
}

pub struct LocalHealthcheckJob {
    session: Uuid,
    reference: PlanRef,
    row_revision: u64,
    request_id: Uuid,
    max_pixels: u32,
    profile: DatasetProfileJob,
}

pub struct LocalHealthcheckRuntime {
    store: Arc<WorkflowStore>,
    interpreter: PathBuf,
    entrypoint: PathBuf,
}

impl LocalHealthcheckRuntime {
    pub fn template_id() -> &'static str {
        DATASET_HEALTHCHECK.id
    }

    pub fn new(
        owner: Arc<WorkspaceThread>,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, WorkflowError> {
        Ok(Self {
            store: Arc::new(WorkflowStore::new(owner)?),
            interpreter,
            entrypoint,
        })
    }

    pub fn admit(
        &self,
        session: Uuid,
        request: LocalHealthcheckRequest,
    ) -> Result<LocalHealthcheckAcceptance, WorkflowError> {
        if request.sample_limit == 0 || request.max_pixels == 0 {
            return Err(WorkflowError::InvalidInputScope);
        }
        let policy = local_policy();
        let prepared = input::freeze_dataset(&self.store, session, &request)?;
        let now = now_ms().map_err(execution)?;
        let draft = self.store.admit(session, prepared, now)?;
        let current = if draft.status == WorkflowStatus::Draft {
            match self.store.accept_default(
                session,
                draft.run_id,
                draft.row_revision,
                &policy,
                request.sample_limit,
                now,
            ) {
                Err(WorkflowError::RevisionConflict) => {
                    let current = self.store.get(session, draft.run_id)?;
                    if current.status == WorkflowStatus::Draft {
                        return Err(WorkflowError::RevisionConflict);
                    }
                    current
                }
                attempt => attempt?,
            }
        } else {
            draft
        };
        if current.status != WorkflowStatus::Ready {
            return Ok(LocalHealthcheckAcceptance {
                workflow: current,
                job: None,
            });
        }
        let reference = current.plan_ref.ok_or(WorkflowError::InvalidRecord)?;
        let accepted = self.store.start_profile(
            session,
            &policy,
            WorkflowProfileRequest {
                reference: reference.clone(),
                expected_row_revision: current.row_revision,
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                max_pixels: request.max_pixels,
            },
            now,
        )?;
        let job = accepted.job.map(|profile| LocalHealthcheckJob {
            session,
            reference,
            row_revision: accepted.workflow.row_revision,
            request_id: request.request_id,
            max_pixels: request.max_pixels,
            profile,
        });
        Ok(LocalHealthcheckAcceptance {
            workflow: accepted.workflow,
            job,
        })
    }

    pub async fn run(&self, job: LocalHealthcheckJob) -> Result<WorkflowReport, WorkflowError> {
        let session = job.session;
        let run = job.reference.run_id;
        match self.execute(job).await {
            Ok(report) => Ok(report),
            Err(failed) => {
                let store = Arc::clone(&self.store);
                tokio::task::spawn_blocking(move || {
                    fail_current_run(&store, session, run, failed.code)
                })
                .await
                .map_err(execution)??;
                Err(failed.error)
            }
        }
    }

    async fn execute(&self, job: LocalHealthcheckJob) -> Result<WorkflowReport, RunFailure> {
        let session = job.session;
        let mut profile = DatasetProfileRuntime::launch(
            Arc::clone(&self.store.owner),
            session,
            self.interpreter.clone(),
            self.entrypoint.clone(),
        )
        .await
        .map_err(|error| RunFailure::worker(error, "WORKER_START_FAILED"))?;
        let outcome = profile.run(job.profile).await;
        let closed = profile.shutdown().await;
        let completed = outcome.map_err(|error| RunFailure::worker(error, "WORKER_INTERRUPTED"))?;
        closed.map_err(|error| RunFailure::worker(error, "WORKER_INTERRUPTED"))?;
        if completed.status != "SUCCEEDED" {
            return Err(RunFailure {
                error: WorkflowError::UpstreamNotReady,
                code: "PROFILE_FAILED",
            });
        }

        let reference = job.reference.clone();
        let store = Arc::clone(&self.store);
        let checked = tokio::task::spawn_blocking(move || {
            store.start_geometry(
                session,
                &local_policy(),
                WorkflowGeometryRequest {
                    reference,
                    expected_row_revision: job.row_revision,
                    request_id: job.request_id,
                    idempotency_key: Uuid::new_v4(),
                    max_pixels: job.max_pixels,
                },
                now_ms().map_err(execution)?,
            )
        })
        .await
        .map_err(|error| RunFailure::workflow(execution(error)))?
        .map_err(RunFailure::workflow)?;
        if !checked.jobs.is_empty() {
            let mut geometry = GeometryRuntime::launch(
                Arc::clone(&self.store.owner),
                session,
                self.interpreter.clone(),
                self.entrypoint.clone(),
            )
            .await
            .map_err(|error| RunFailure::worker(error, "WORKER_START_FAILED"))?;
            for job in checked.jobs {
                if let Err(error) = geometry.audit_admitted(job).await {
                    let _ = geometry.terminate().await;
                    return Err(RunFailure::worker(error, "WORKER_INTERRUPTED"));
                }
            }
            geometry
                .shutdown()
                .await
                .map_err(|error| RunFailure::worker(error, "WORKER_INTERRUPTED"))?;
        }
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            store.build_report(
                session,
                &local_policy(),
                WorkflowReportRequest {
                    reference: job.reference,
                    expected_row_revision: checked.workflow.row_revision,
                    request_id: job.request_id,
                    idempotency_key: Uuid::new_v4(),
                },
                now_ms().map_err(execution)?,
            )
        })
        .await
        .map_err(|error| RunFailure::workflow(execution(error)))?
        .map_err(RunFailure::workflow)
    }
}

struct RunFailure {
    error: WorkflowError,
    code: &'static str,
}

impl RunFailure {
    fn worker(error: crate::worker_runtime::WorkerRuntimeError, code: &'static str) -> Self {
        Self {
            error: execution(error),
            code,
        }
    }

    fn workflow(error: WorkflowError) -> Self {
        Self {
            error,
            code: "WORKFLOW_EXECUTION_FAILED",
        }
    }
}

fn fail_current_run(
    store: &WorkflowStore,
    session: Uuid,
    run: Uuid,
    code: &'static str,
) -> Result<(), WorkflowError> {
    let now = now_ms().map_err(execution)?;
    let status = if code == "WORKER_INTERRUPTED" {
        "INTERRUPTED"
    } else {
        "FAILED"
    };
    store.owner.workspace_action(session, move |owner| {
        let transaction = owner.connection.transaction().map_err(database)?;
        let run = run.to_string();
        let session = session.to_string();
        let failed = transaction.execute(
            "UPDATE workflow_runs SET status=?5,error_code=?3,finished_at_ms=?4,
                updated_at_ms=?4,row_revision=row_revision+1
             WHERE workflow_run_id=?1 AND workspace_session_id=?2 AND status='RUNNING'",
            params![&run,&session,code,now,status],
        ).map_err(database)?;
        if failed == 1 {
            transaction.execute(
                "UPDATE tasks SET status=?5,error_code=?3,finished_at_ms=?4,updated_at_ms=?4,
                    row_revision=row_revision+1,execution_metadata_json=json_set(execution_metadata_json,'$.grant_state','REVOKED')
                 WHERE workflow_run_id=?1 AND workspace_session_id=?2 AND status IN ('QUEUED','DISPATCHING','RUNNING','CANCELLING')",
                params![&run,&session,code,now,status],
            ).map_err(database)?;
        }
        transaction.commit().map_err(database)
    }).map_err(WorkflowError::Storage)
}

fn local_policy() -> PlanPolicy {
    PlanPolicy {
        available_steps: BTreeSet::from([Step::DatasetProfile, Step::AnnotationGeometry]),
        allowed_model_profiles: BTreeSet::new(),
        max_sample_limit: u32::MAX,
        max_adjustments: 0,
        max_model_calls: 0,
        max_output_tokens: 0,
    }
}

fn execution(error: impl std::error::Error + Send + Sync + 'static) -> WorkflowError {
    WorkflowError::Execution(Box::new(error))
}
