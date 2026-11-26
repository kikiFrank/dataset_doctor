//! Admit a report export through the trusted Workspace command host.

use std::sync::Arc;

use dd_infra::artifact_reference::{ArtifactOwner as StoredOwner, ArtifactReference as StoredRef};
use dd_infra::report_export::{
    ReportExportError, ReportExportFormat, ReportExportRequest, ReportExportService,
};
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::report_export::{
    ReportExportFormat as WireFormat, ReportExportSubmitRequest, ReportExportView,
};
use dd_ui_contracts::workspace::AppError;
use dd_ui_contracts::{ArtifactOwner, DecimalU64};
use tauri::State;
use uuid::Uuid;

use super::{Commands, run};

#[tauri::command]
pub async fn report_export_submit(
    request: ReportExportSubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ImportOperationRef, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.submit_report_export(request),
    )
    .await
}

impl Commands {
    pub(super) fn submit_report_export(
        &self,
        request: ReportExportSubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let reference = request.report_ref;
        let ArtifactOwner::Task(task) = reference.owner else {
            return Err(self.error("INVALID_ARGUMENT", "请选择已有任务生成的质量报告。"));
        };
        let format = match request.format {
            WireFormat::Json => ReportExportFormat::Json,
            WireFormat::Markdown => ReportExportFormat::Markdown,
        };
        let service = self.report_export_service()?;
        let (task, job) = service
            .submit(
                session,
                ReportExportRequest {
                    report_ref: StoredRef {
                        artifact_id: self.parse_uuid(reference.artifact_id)?,
                        owner: StoredOwner::Task(self.parse_uuid(task)?),
                        kind: reference.kind,
                        media_type: reference.media_type,
                        sha256: reference.sha256,
                        size_bytes: reference.size_bytes.into(),
                        schema_id: reference.schema_id,
                    },
                    format,
                    destination_grant_id: self.parse_uuid(request.destination_grant_id)?,
                    request_id,
                    idempotency_key,
                },
            )
            .map_err(|error| self.report_export_error(error))?;
        let operation = self.import_operation(
            ImportOperationKind::ReportExport,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )?;
        if let Some(job) = job {
            tauri::async_runtime::spawn_blocking(move || {
                let _ = service.run(session, job);
            });
        }
        Ok(operation)
    }

    pub(super) fn report_export_task(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let task = self
            .report_export_service()?
            .get(session, task)
            .map_err(|error| self.report_export_error(error))?;
        let receipt = task
            .receipt
            .map(|receipt| {
                let format = match receipt.format {
                    ReportExportFormat::Json => WireFormat::Json,
                    ReportExportFormat::Markdown => WireFormat::Markdown,
                };
                let size_bytes = DecimalU64::try_from(receipt.size_bytes)
                    .map_err(|_| self.error("DATA_CORRUPTION", "报告导出的文件大小无效。"))?;
                Ok(ReportExportView {
                    report_ref: self.wire_artifact(receipt.report_ref)?,
                    format,
                    file_name: receipt.file_name,
                    sha256: receipt.sha256,
                    size_bytes,
                })
            })
            .transpose()?;
        Ok(ImportTaskView {
            task_id: self.uuid_text(task.task_id.to_string())?,
            operation_kind: ImportOperationKind::ReportExport,
            status: task.status,
            error_code: task.error_code,
            report_export: receipt,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            workspace_backup: None,
            video_frame: None,
            video_sample: None,
            video_tracking: None,
            staging_commit: None,
            color_preview: None,
            color_apply: None,
        })
    }

    fn report_export_service(&self) -> Result<ReportExportService, AppError> {
        ReportExportService::new(Arc::clone(&self.thread))
            .map_err(|error| self.report_export_error(error))
    }

    fn report_export_error(&self, error: ReportExportError) -> AppError {
        match error {
            ReportExportError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "此导出请求的内容已变化，请重新选择目录。"),
            ReportExportError::Workspace(error) => self.map_error(error),
            ReportExportError::InvalidTask => {
                self.error("NOT_FOUND", "当前工作区中没有此报告导出任务。")
            }
            ReportExportError::InvalidReport => {
                self.error("INVALID_ARGUMENT", "报告引用无效或内容已变化。")
            }
            _ => self.error("INTERNAL", "无法受理或查询报告导出。"),
        }
    }
}
