use std::sync::Arc;

use dd_infra::worker_process::WorkerProgram;
use dd_infra::workflow_store::{
    LocalHealthcheckAcceptance, LocalHealthcheckRequest, LocalHealthcheckRuntime, PlanSource,
    WorkflowError, WorkflowRecheckRequest, WorkflowResumeRequest as ResumeBinding,
    WorkflowSnapshot, WorkflowStatus, WorkflowStore,
};
use dd_ui_contracts::workflow::{
    WorkflowCompleteReviewRequest, WorkflowControlAction, WorkflowControlReceipt,
    WorkflowGeometryTaskView, WorkflowGetRequest, WorkflowListRequest, WorkflowPageView,
    WorkflowPauseRequest, WorkflowPlanSourceView, WorkflowReportGetRequest,
    WorkflowReportScopeView, WorkflowReportView, WorkflowResumeRequest, WorkflowResumeStrategy,
    WorkflowStartRequest, WorkflowStatusView, WorkflowTaskView, WorkflowView,
};
use dd_ui_contracts::workspace::AppError;
use tauri::State;

use super::{Commands, WorkspaceThread, run};
use crate::RuntimeMode;

pub(crate) fn configured(
    owner: Arc<WorkspaceThread>,
    program: Option<WorkerProgram>,
) -> Option<Arc<LocalHealthcheckRuntime>> {
    LocalHealthcheckRuntime::with_program(owner, program?)
        .ok()
        .map(Arc::new)
}

#[tauri::command]
pub async fn workflow_start(
    request: WorkflowStartRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runtime: State<'_, Option<Arc<LocalHealthcheckRuntime>>>,
    mode: State<'_, RuntimeMode>,
) -> Result<WorkflowView, AppError> {
    let runtime = runtime.inner().clone();
    let hold_initial_dispatch = mode.smoke_workflow_controls;
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            let runtime = runtime
                .ok_or_else(|| commands.error("WORKER_UNAVAILABLE", "本机 Worker 未就绪。"))?;
            if hold_initial_dispatch {
                // The control smoke pauses an admitted task before its first dispatch.
                let accepted = commands.admit_workflow(&runtime, request)?;
                commands.workflow_view(accepted.workflow)
            } else {
                commands.start_workflow(runtime, request)
            }
        },
    )
    .await
}

#[tauri::command]
pub async fn workflow_get(
    request: WorkflowGetRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<WorkflowView, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.get_workflow(request),
    )
    .await
}

#[tauri::command]
pub async fn workflow_complete_review(
    request: WorkflowCompleteReviewRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runtime: State<'_, Option<Arc<LocalHealthcheckRuntime>>>,
) -> Result<WorkflowView, AppError> {
    let runtime = runtime.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            let runtime = runtime
                .ok_or_else(|| commands.error("WORKER_UNAVAILABLE", "本机 Worker 未就绪。"))?;
            commands.complete_workflow_review(runtime, request)
        },
    )
    .await
}

#[tauri::command]
pub async fn workflow_pause(
    request: WorkflowPauseRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<WorkflowControlReceipt, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.pause_workflow(request),
    )
    .await
}

#[tauri::command]
pub async fn workflow_resume(
    request: WorkflowResumeRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runtime: State<'_, Option<Arc<LocalHealthcheckRuntime>>>,
) -> Result<WorkflowControlReceipt, AppError> {
    let runtime = runtime.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            let runtime = runtime
                .ok_or_else(|| commands.error("WORKER_UNAVAILABLE", "本机 Worker 未就绪。"))?;
            commands.resume_workflow(runtime, request)
        },
    )
    .await
}

#[tauri::command]
pub async fn workflow_list(
    request: WorkflowListRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<WorkflowPageView, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.list_workflows(request),
    )
    .await
}

impl Commands {
    pub(super) fn complete_workflow_review(
        &self,
        runtime: Arc<LocalHealthcheckRuntime>,
        request: WorkflowCompleteReviewRequest,
    ) -> Result<WorkflowView, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let run = self.parse_uuid(request.workflow_run_id)?;
        let expected_report = serde_json::from_value(
            serde_json::to_value(request.expected_report)
                .map_err(|_| self.error("INVALID_ARGUMENT", "报告引用无效。"))?,
        )
        .map_err(|_| self.error("INVALID_ARGUMENT", "报告引用无效。"))?;
        let job = runtime
            .prepare_review_completion(
                session,
                WorkflowRecheckRequest {
                    workflow_run_id: run,
                    expected_row_revision: request.expected_revision.into(),
                    expected_report,
                    request_id,
                    idempotency_key,
                    acknowledged_incomplete_checks: request
                        .acknowledged_incomplete_checks
                        .into_iter()
                        .map(
                            |entry| dd_infra::workflow_store::WorkflowReviewCheckAcknowledgment {
                                check_id: entry.check_id,
                                reason: entry.reason,
                            },
                        )
                        .collect(),
                },
            )
            .map_err(|error| self.workflow_error(error))?;
        let snapshot = self
            .workflow_store()?
            .get(session, run)
            .map_err(|error| self.workflow_error(error))?;
        if snapshot.status != WorkflowStatus::Completed {
            tauri::async_runtime::spawn(async move {
                let _ = runtime.run_review_completion(job).await;
            });
        }
        self.workflow_view(snapshot)
    }

    pub(super) fn start_workflow(
        &self,
        runtime: Arc<LocalHealthcheckRuntime>,
        request: WorkflowStartRequest,
    ) -> Result<WorkflowView, AppError> {
        let accepted = self.admit_workflow(&runtime, request)?;
        self.workflow_view(schedule_workflow(runtime, accepted))
    }

    fn admit_workflow(
        &self,
        runtime: &LocalHealthcheckRuntime,
        request: WorkflowStartRequest,
    ) -> Result<LocalHealthcheckAcceptance, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        if request.template_id != LocalHealthcheckRuntime::template_id() {
            return Err(self.error("INVALID_ARGUMENT", "请选择已接入的本地数据体检模板。"));
        }
        runtime
            .admit(
                session,
                LocalHealthcheckRequest {
                    dataset_id: self.parse_uuid(request.scope.dataset_id)?,
                    sample_limit: request.policy_overrides.sample_limit.unwrap_or(100),
                    max_pixels: super::geometry::MAX_IMAGE_PIXELS as u32,
                    request_id,
                    idempotency_key,
                },
            )
            .map_err(|error| self.workflow_error(error))
    }

    pub(super) fn pause_workflow(
        &self,
        request: WorkflowPauseRequest,
    ) -> Result<WorkflowControlReceipt, AppError> {
        let session = self.session(request.context)?;
        let run = self.parse_uuid(request.workflow_run_id.clone())?;
        let control = self
            .workflow_store()?
            .pause(
                session,
                run,
                request.expected_revision.into(),
                self.now_ms()?,
            )
            .map_err(|error| self.workflow_error(error))?;
        Ok(WorkflowControlReceipt {
            workflow_run_id: request.workflow_run_id,
            requested: WorkflowControlAction::Pause,
            observed_status: workflow_status(control.observed_status),
            revision: control.revision.into(),
            blocking_task_ids: control
                .blocking_task_ids
                .into_iter()
                .map(|task| self.uuid_text(task.to_string()))
                .collect::<Result<_, _>>()?,
        })
    }

    pub(super) fn resume_workflow(
        &self,
        runtime: Arc<LocalHealthcheckRuntime>,
        request: WorkflowResumeRequest,
    ) -> Result<WorkflowControlReceipt, AppError> {
        let WorkflowResumeStrategy::ResumePaused = request.strategy;
        let request_id = self.parse_uuid(request.context.request_id.clone())?;
        let session = self.session(request.context)?;
        let run = self.parse_uuid(request.workflow_run_id.clone())?;
        let current = self
            .workflow_store()?
            .get(session, run)
            .map_err(|error| self.workflow_error(error))?;
        let plan = current
            .plan_ref
            .ok_or_else(|| self.workflow_error(WorkflowError::ControlUnavailable))?;
        let accepted = runtime
            .resume(
                session,
                ResumeBinding {
                    reference: plan,
                    expected_row_revision: request.expected_revision.into(),
                },
                request_id,
            )
            .map_err(|error| self.workflow_error(error))?;
        let snapshot = schedule_workflow(runtime, accepted);
        Ok(WorkflowControlReceipt {
            workflow_run_id: request.workflow_run_id,
            requested: WorkflowControlAction::Resume,
            observed_status: workflow_status(snapshot.status),
            revision: snapshot.row_revision.into(),
            blocking_task_ids: Vec::new(),
        })
    }

    pub(super) fn get_workflow(
        &self,
        request: WorkflowGetRequest,
    ) -> Result<WorkflowView, AppError> {
        let session = self.session(request.context)?;
        let run = self.parse_uuid(request.workflow_run_id)?;
        let snapshot = self
            .workflow_store()?
            .get(session, run)
            .map_err(|error| self.workflow_error(error))?;
        self.workflow_view(snapshot)
    }

    pub(super) fn get_workflow_report(
        &self,
        request: WorkflowReportGetRequest,
    ) -> Result<WorkflowReportView, AppError> {
        let session = self.session(request.context)?;
        let run = self.parse_uuid(request.workflow_run_id)?;
        let report = self
            .workflow_store()?
            .read_report(session, run)
            .map_err(|error| self.workflow_error(error))?;
        let data = report.data;
        Ok(WorkflowReportView {
            report_id: self.uuid_text(data.report_id.to_string())?,
            scope: WorkflowReportScopeView {
                dataset_id: self.uuid_text(data.scope.dataset_id.to_string())?,
                workflow_run_id: self.uuid_text(run.to_string())?,
            },
            input_fingerprint: data.input_fingerprint,
            execution_coverage: data.execution_coverage,
            findings_by_severity: data.findings_by_severity,
            human_ack_status: data.human_ack_status,
            unexecuted_checks: data.unexecuted_checks,
            repair_outcomes: data.repair_outcomes,
            checks: self.report_checks_view(data.checks)?,
            report_artifact_ref: self.wire_artifact(report.reference)?,
        })
    }

    fn list_workflows(&self, request: WorkflowListRequest) -> Result<WorkflowPageView, AppError> {
        let session = self.session(request.context)?;
        let dataset = self.parse_uuid(request.dataset_id)?;
        let cursor = request
            .cursor
            .map(|value| self.parse_uuid(value))
            .transpose()?;
        let page = self
            .workflow_store()?
            .list(session, dataset, cursor, request.page_size)
            .map_err(|error| self.workflow_error(error))?;
        Ok(WorkflowPageView {
            items: page
                .items
                .into_iter()
                .map(|view| self.workflow_view(view))
                .collect::<Result<_, _>>()?,
            next_cursor: page
                .next_cursor
                .map(|id| self.uuid_text(id.to_string()))
                .transpose()?,
        })
    }

    fn workflow_store(&self) -> Result<WorkflowStore, AppError> {
        WorkflowStore::new(Arc::clone(&self.thread)).map_err(|error| self.workflow_error(error))
    }

    fn workflow_view(&self, snapshot: WorkflowSnapshot) -> Result<WorkflowView, AppError> {
        let id = |id: uuid::Uuid| self.uuid_text(id.to_string());
        Ok(WorkflowView {
            workflow_run_id: id(snapshot.run_id)?,
            planning_task_id: id(snapshot.planning_task_id)?,
            status: workflow_status(snapshot.status),
            row_revision: snapshot.row_revision.into(),
            plan_revision: snapshot
                .plan_ref
                .as_ref()
                .map(|reference| reference.plan_revision.into()),
            input_fingerprint: snapshot
                .plan_ref
                .map(|reference| dd_infra::task_store::format_digest(&reference.input_fingerprint)),
            plan_source: snapshot.plan_source.map(|source| match source {
                PlanSource::LocalDefault => WorkflowPlanSourceView::LocalDefault,
                PlanSource::Proposed => WorkflowPlanSourceView::Proposed,
            }),
            profile_task: snapshot
                .profile_task
                .map(|task| {
                    Ok(WorkflowTaskView {
                        task_id: id(task.task_id)?,
                        status: task.status,
                    })
                })
                .transpose()?,
            geometry_tasks: snapshot
                .geometry_tasks
                .into_iter()
                .map(|task| {
                    Ok(WorkflowGeometryTaskView {
                        asset_id: id(task.asset_id)?,
                        task_id: id(task.task_id)?,
                        status: task.status,
                    })
                })
                .collect::<Result<_, AppError>>()?,
            report_task_id: snapshot.report_task_id.map(id).transpose()?,
            review_recheck_tasks: snapshot
                .review_recheck_tasks
                .into_iter()
                .map(|task| {
                    Ok(WorkflowGeometryTaskView {
                        asset_id: id(task.asset_id)?,
                        task_id: id(task.task_id)?,
                        status: task.status,
                    })
                })
                .collect::<Result<_, AppError>>()?,
            error_code: snapshot.error_code,
        })
    }

    fn workflow_error(&self, error: WorkflowError) -> AppError {
        match error {
            WorkflowError::Storage(error) => self.dataset_error(error),
            WorkflowError::UnknownRun => self.error("NOT_FOUND", "当前工作区没有此体检记录。"),
            WorkflowError::UpstreamNotReady => {
                self.error("FAILED_PRECONDITION", "体检报告尚未生成。")
            }
            WorkflowError::Paused | WorkflowError::ControlUnavailable => {
                self.error("FAILED_PRECONDITION", "体检当前处于暂停状态或无法暂停。")
            }
            WorkflowError::InputChanged => {
                self.error("INPUT_CHANGED", "体检输入已变化，请刷新数据集后重新提交。")
            }
            WorkflowError::RevisionConflict | WorkflowError::IdempotencyConflict => {
                self.error("CONFLICT", "体检状态或提交参数已变化，请刷新后重试。")
            }
            WorkflowError::InvalidInputScope | WorkflowError::Plan(_) => {
                self.error("INVALID_ARGUMENT", "体检范围或抽样设置无效。")
            }
            WorkflowError::InvalidRecord | WorkflowError::Json(_) => {
                self.error("DATA_CORRUPTION", "无法核验已保存的体检记录。")
            }
            _ => self.error("INTERNAL", "无法处理体检请求。"),
        }
    }
}

fn schedule_workflow(
    runtime: Arc<LocalHealthcheckRuntime>,
    accepted: LocalHealthcheckAcceptance,
) -> WorkflowSnapshot {
    if let Some(job) = accepted.job {
        tauri::async_runtime::spawn(async move {
            let _ = runtime.run(job).await;
        });
    }
    accepted.workflow
}

fn workflow_status(status: WorkflowStatus) -> WorkflowStatusView {
    match status {
        WorkflowStatus::Draft => WorkflowStatusView::Draft,
        WorkflowStatus::Ready => WorkflowStatusView::Ready,
        WorkflowStatus::Running => WorkflowStatusView::Running,
        WorkflowStatus::Paused => WorkflowStatusView::Paused,
        WorkflowStatus::Interrupted => WorkflowStatusView::Interrupted,
        WorkflowStatus::WaitingReview => WorkflowStatusView::WaitingReview,
        WorkflowStatus::Completed => WorkflowStatusView::Completed,
        WorkflowStatus::Failed => WorkflowStatusView::Failed,
    }
}
