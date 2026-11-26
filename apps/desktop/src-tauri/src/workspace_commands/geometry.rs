use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dd_infra::geometry_runtime::{
    GeometryAuditJob, GeometryAuditRequest, GeometryRuntime, GeometryRuntimeError,
    read_geometry_report,
};
use dd_infra::staging_report::{OptionalStagingCheck, StagingReportRequest, StagingReportService};
use dd_infra::task_store::TaskStoreError;
use dd_infra::worker_process::WorkerProgram;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::dataset_import::{
    GeometryFindingView, GeometryReportView, GeometrySubmitRequest, ImportOperationKind,
    ImportOperationRef, ImportTaskView,
};
use dd_ui_contracts::{DecimalU64, workspace::AppError};
use tauri::State;
use uuid::Uuid;

use super::{Commands, run};

pub(super) const MAX_IMAGE_PIXELS: u64 = 100_000_000;

pub(crate) struct GeometryRunner {
    program: WorkerProgram,
    active: Mutex<Option<Uuid>>,
}

impl GeometryRunner {
    #[cfg(test)]
    pub(crate) fn new(interpreter: PathBuf, entrypoint: PathBuf) -> Arc<Self> {
        Self::from_program(WorkerProgram::Script {
            interpreter,
            entrypoint,
        })
    }

    fn from_program(program: WorkerProgram) -> Arc<Self> {
        Arc::new(Self {
            program,
            active: Mutex::new(None),
        })
    }

    pub(crate) fn configured(resource_dir: Option<PathBuf>) -> Option<Arc<Self>> {
        super::configured_worker_program(resource_dir).map(Self::from_program)
    }

    pub(crate) fn wait_for_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.active.lock() {
                Ok(active) if active.is_none() => return true,
                Ok(_) if std::time::Instant::now() < deadline => (),
                _ => return false,
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    async fn execute(
        self: Arc<Self>,
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        job: GeometryAuditJob,
        staging_report: StagingReportRequest,
    ) {
        let _reservation = Reservation(Arc::clone(&self));
        let task = job.task().task_id;
        let worker = GeometryRuntime::launch_with_program(
            Arc::clone(&workspace),
            session,
            self.program.clone(),
        )
        .await;
        let startup_failed = match worker {
            Ok(mut worker) => {
                if let Ok(outcome) = worker.audit_admitted(job).await {
                    let _ = worker.shutdown().await;
                    if outcome.status == "SUCCEEDED" {
                        let _ = tauri::async_runtime::spawn_blocking(move || {
                            StagingReportService::build_after_geometry(
                                workspace,
                                session,
                                staging_report,
                            )
                        })
                        .await;
                    }
                    return;
                }
                if worker.terminate().await.is_err() {
                    return;
                }
                false
            }
            Err(_) => true,
        };
        let _ = tauri::async_runtime::spawn_blocking(move || {
            GeometryRuntime::record_unavailable(&workspace, session, task, startup_failed)
        })
        .await;
    }
}

struct Reservation(Arc<GeometryRunner>);

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active = None;
        }
    }
}

#[tauri::command]
pub async fn annotation_geometry_submit(
    request: GeometrySubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runner: State<'_, Option<Arc<GeometryRunner>>>,
) -> Result<ImportOperationRef, AppError> {
    let runner = runner.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            let runner = runner.ok_or_else(|| {
                commands.error("FAILED_PRECONDITION", "本机检查 Worker 尚未配置。")
            })?;
            commands.start_geometry(runner, request)
        },
    )
    .await
}

impl Commands {
    pub(super) fn start_geometry(
        &self,
        runner: Arc<GeometryRunner>,
        request: GeometrySubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let dataset_id = self.parse_uuid(request.dataset_id)?;
        let asset_id = self.parse_uuid(request.asset_id)?;
        let mut active = runner
            .active
            .lock()
            .map_err(|_| self.error("INTERNAL", "检查任务不可用。"))?;
        if active.is_some_and(|current| current != idempotency_key) {
            return Err(self.error("RESOURCE_BUSY", "已有图片检查正在运行，请稍后再试。"));
        }
        let job = GeometryRuntime::admit(
            &self.thread,
            session,
            GeometryAuditRequest {
                dataset_id,
                asset_id,
                request_id,
                idempotency_key,
                max_pixels: MAX_IMAGE_PIXELS,
            },
        )
        .map_err(|error| match error {
            GeometryRuntimeError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "检查请求摘要已变化。"),
            GeometryRuntimeError::Workspace(error) => self.dataset_error(error),
            _ => self.error("INTERNAL", "无法受理图片检查。"),
        })?;
        let operation = self.import_operation(
            ImportOperationKind::AnnotationGeometry,
            job.task().task_id,
            job.task().status.clone(),
            job.task().created_at_ms,
        )?;
        if job.task().status == "QUEUED"
            && job.task().workspace_session == session
            && active.is_none()
        {
            *active = Some(idempotency_key);
            drop(active);
            let workspace = Arc::clone(&self.thread);
            let report = StagingReportRequest {
                request_id,
                idempotency_key: job.task().task_id,
                target_dataset_id: dataset_id,
                asset_id,
                geometry_task_id: job.task().task_id,
                optional_checks: vec![OptionalStagingCheck::SemanticReview],
            };
            tauri::async_runtime::spawn(runner.execute(workspace, session, job, report));
        }
        Ok(operation)
    }

    pub(super) fn geometry_task(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let outcome = GeometryRuntime::task_outcome(&self.thread, session, task).map_err(
            |error| match error {
                WorkspaceThreadError::Task(TaskStoreError::UnknownTask) => {
                    self.error("NOT_FOUND", "当前工作区中没有此任务。")
                }
                other => self.dataset_error(other),
            },
        )?;
        let geometry_report = if outcome.status == "SUCCEEDED" {
            let report = read_geometry_report(&self.thread, session, task)
                .map_err(|_| self.error("FAILED_PRECONDITION", "无法校验检查报告，请重新检查。"))?;
            Some(GeometryReportView {
                status: report.status,
                width: report.image.width,
                height: report.image.height,
                image_digest: report.image.sha256,
                findings: report
                    .findings
                    .into_iter()
                    .map(|finding| GeometryFindingView {
                        code: finding.code,
                        annotation_id: finding.annotation_id.map(DecimalU64::from),
                    })
                    .collect(),
            })
        } else {
            None
        };
        Ok(ImportTaskView {
            color_preview: None,
            color_apply: None,
            staging_commit: None,
            video_tracking: None,
            task_id: self.uuid_text(task.to_string())?,
            operation_kind: ImportOperationKind::AnnotationGeometry,
            status: outcome.status,
            error_code: outcome.error_code,
            video_sample: None,
            video_frame: None,
            preview: None,
            registration: None,
            geometry_report,
            coco_export: None,
            report_export: None,
            workspace_backup: None,
        })
    }
}
