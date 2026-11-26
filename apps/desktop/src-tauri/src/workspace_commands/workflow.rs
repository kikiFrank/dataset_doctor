use std::sync::Arc;

use dd_infra::workflow_store::{
    LocalHealthcheckRequest, LocalHealthcheckRuntime, PlanSource, WorkflowError, WorkflowSnapshot,
    WorkflowStatus, WorkflowStore,
};
use dd_ui_contracts::workflow::{
    WorkflowGeometryTaskView, WorkflowGetRequest, WorkflowPlanSourceView, WorkflowReportGetRequest,
    WorkflowReportScopeView, WorkflowReportView, WorkflowStartRequest, WorkflowStatusView,
    WorkflowTaskView, WorkflowView,
};
use dd_ui_contracts::workspace::AppError;
use tauri::State;

use super::{Commands, WorkspaceThread, run};

pub(crate) fn configured(owner: Arc<WorkspaceThread>) -> Option<Arc<LocalHealthcheckRuntime>> {
    let (interpreter, entrypoint) = super::configured_worker_script()?;
    LocalHealthcheckRuntime::new(owner, interpreter, entrypoint)
        .ok()
        .map(Arc::new)
}

#[tauri::command]
pub async fn workflow_start(
    request: WorkflowStartRequest,
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
            commands.start_workflow(runtime, request)
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

impl Commands {
    pub(super) fn start_workflow(
        &self,
        runtime: Arc<LocalHealthcheckRuntime>,
        request: WorkflowStartRequest,
    ) -> Result<WorkflowView, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        if request.template_id != LocalHealthcheckRuntime::template_id() {
            return Err(self.error("INVALID_ARGUMENT", "请选择已接入的本地数据体检模板。"));
        }
        let accepted = runtime
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
            .map_err(|error| self.workflow_error(error))?;
        if let Some(job) = accepted.job {
            tauri::async_runtime::spawn(async move {
                let _ = runtime.run(job).await;
            });
        }
        self.workflow_view(accepted.workflow)
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

    fn workflow_store(&self) -> Result<WorkflowStore, AppError> {
        WorkflowStore::new(Arc::clone(&self.thread)).map_err(|error| self.workflow_error(error))
    }

    fn workflow_view(&self, snapshot: WorkflowSnapshot) -> Result<WorkflowView, AppError> {
        let id = |id: uuid::Uuid| self.uuid_text(id.to_string());
        Ok(WorkflowView {
            workflow_run_id: id(snapshot.run_id)?,
            planning_task_id: id(snapshot.planning_task_id)?,
            status: match snapshot.status {
                WorkflowStatus::Draft => WorkflowStatusView::Draft,
                WorkflowStatus::Ready => WorkflowStatusView::Ready,
                WorkflowStatus::Running => WorkflowStatusView::Running,
                WorkflowStatus::Interrupted => WorkflowStatusView::Interrupted,
                WorkflowStatus::WaitingReview => WorkflowStatusView::WaitingReview,
                WorkflowStatus::Failed => WorkflowStatusView::Failed,
            },
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
