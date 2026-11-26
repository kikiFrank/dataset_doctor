use std::path::PathBuf;
use std::sync::Arc;

use dd_infra::coco_export::{
    CocoExportError, CocoExportPolicy, CocoExportRequest, CocoExportService, CocoExportSubmission,
};
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::dataset_export::{CocoExportView, DatasetExportSubmitRequest, ExportPolicy};
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::workspace::{AppError, CommandContext, PathGrant};
use tauri::State;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::{Commands, GRANT_LIFETIME, run};

#[tauri::command]
pub async fn dataset_export_submit(
    request: DatasetExportSubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ImportOperationRef, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.submit_export(request),
    )
    .await
}

impl Commands {
    pub(super) fn export_directory(
        &self,
        context: CommandContext,
        path: PathBuf,
    ) -> Result<PathGrant, AppError> {
        let session = self.session(context)?;
        let display_path = path.to_string_lossy().into_owned();
        let grant = self
            .thread
            .grant_export_directory(session, path)
            .map_err(|error| self.map_error(error))?;
        let expires_at = (OffsetDateTime::now_utc() + GRANT_LIFETIME)
            .format(&Rfc3339)
            .map_err(|_| self.error("INTERNAL", "无法生成目录授权。"))?;
        Ok(PathGrant {
            grant_id: self.uuid_text(grant.grant_id.to_string())?,
            display_path,
            allowed_operations: vec!["DATASET_EXPORT".into()],
            expires_at,
        })
    }

    pub(super) fn submit_export(
        &self,
        request: DatasetExportSubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let policy = match request.export_policy {
            ExportPolicy::Strict => CocoExportPolicy::Strict,
            ExportPolicy::ExcludeIneligible => CocoExportPolicy::ExcludeIneligible,
        };
        let service = self.export_service()?;
        let (task, job) = service
            .submit(
                session,
                CocoExportSubmission {
                    request: CocoExportRequest {
                        dataset_id: self.parse_uuid(request.dataset_id)?,
                        destination_grant_id: self.parse_uuid(request.destination_grant_id)?,
                        policy,
                    },
                    request_id,
                    idempotency_key,
                },
                self.now_ms()?,
            )
            .map_err(|error| self.export_error(error))?;
        let operation = self.import_operation(
            ImportOperationKind::DatasetExport,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )?;
        if let Some(job) = job {
            tauri::async_runtime::spawn_blocking(move || {
                let _ = service.run_admitted(session, job, || {
                    OffsetDateTime::now_utc()
                        .unix_timestamp_nanos()
                        .div_euclid(1_000_000) as i64
                });
            });
        }
        Ok(operation)
    }

    pub(super) fn export_task(
        &self,
        session: Uuid,
        task_id: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let task = self
            .export_service()?
            .task_snapshot(session, task_id, self.now_ms()?)
            .map_err(|error| self.export_error(error))?
            .ok_or_else(|| self.error("NOT_FOUND", "当前工作区中没有此任务。"))?;
        let receipt = task.receipt.map(|receipt| CocoExportView {
            package_path: receipt.package.to_string_lossy().into_owned(),
            image_count: (receipt.image_count as u64).into(),
            annotation_count: (receipt.annotation_count as u64).into(),
            excluded_count: (receipt.exclusions.len() as u64).into(),
        });
        Ok(ImportTaskView {
            color_preview: None,
            color_apply: None,
            staging_commit: None,
            video_tracking: None,
            task_id: self.uuid_text(task_id.to_string())?,
            operation_kind: ImportOperationKind::DatasetExport,
            status: task.status,
            error_code: task.error_code,
            video_sample: None,
            video_frame: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: receipt,
            workspace_backup: None,
        })
    }

    fn export_service(&self) -> Result<CocoExportService, AppError> {
        CocoExportService::new(Arc::clone(&self.thread)).map_err(|error| self.export_error(error))
    }

    fn export_error(&self, error: CocoExportError) -> AppError {
        match error {
            CocoExportError::Storage(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "此导出请求的内容已变化，请重新选择目录。"),
            CocoExportError::Storage(error) => self.dataset_error(error),
            _ => self.error("INTERNAL", "无法受理或查询导出。"),
        }
    }
}
