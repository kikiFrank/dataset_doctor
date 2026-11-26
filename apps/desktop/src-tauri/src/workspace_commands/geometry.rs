use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dd_infra::geometry_runtime::{
    GeometryAuditJob, GeometryAuditRequest, GeometryRuntime, GeometryRuntimeError,
    read_geometry_report,
};
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::dataset_import::{
    GeometryFindingView, GeometryReportView, GeometrySubmitRequest, ImportOperationKind,
    ImportOperationRef, ImportTaskView,
};
use dd_ui_contracts::{DecimalU64, workspace::AppError};
use tauri::State;
use uuid::Uuid;

use super::{Commands, run};

const MAX_IMAGE_PIXELS: u64 = 100_000_000;

pub(crate) struct GeometryRunner {
    interpreter: PathBuf,
    entrypoint: PathBuf,
    active: Mutex<Option<Uuid>>,
}

impl GeometryRunner {
    pub(crate) fn new(interpreter: PathBuf, entrypoint: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            interpreter,
            entrypoint,
            active: Mutex::new(None),
        })
    }

    pub(crate) fn configured() -> Option<Arc<Self>> {
        if let (Some(python), Some(entrypoint)) = (
            std::env::var_os("DD_WORKER_PYTHON"),
            std::env::var_os("DD_WORKER_ENTRYPOINT"),
        ) {
            let python = PathBuf::from(python);
            let entrypoint = PathBuf::from(entrypoint);
            if python.is_absolute() && entrypoint.is_absolute() {
                return Some(Self::new(python, entrypoint));
            }
        }
        if !cfg!(debug_assertions) {
            return None;
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let interpreter = if cfg!(windows) {
            repo.join("target/dev/worker-venv/Scripts/python.exe")
        } else {
            repo.join("target/dev/worker-venv/bin/python")
        };
        let entrypoint = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
        Some(Self::new(
            interpreter.canonicalize().ok()?,
            entrypoint.canonicalize().ok()?,
        ))
    }

    async fn execute(
        self: Arc<Self>,
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        job: GeometryAuditJob,
    ) {
        let _reservation = Reservation(Arc::clone(&self));
        let task = job.task().task_id;
        let worker = GeometryRuntime::launch(
            Arc::clone(&workspace),
            session,
            self.interpreter.clone(),
            self.entrypoint.clone(),
        )
        .await;
        let startup_failed = match worker {
            Ok(mut worker) => {
                let audit = worker.audit_admitted(job).await;
                let stopped = worker.shutdown().await;
                if audit.is_ok() || stopped.is_err() {
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
            tauri::async_runtime::spawn(runner.execute(workspace, session, job));
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
            task_id: self.uuid_text(task.to_string())?,
            operation_kind: ImportOperationKind::AnnotationGeometry,
            status: outcome.status,
            error_code: outcome.error_code,
            preview: None,
            registration: None,
            geometry_report,
        })
    }
}
