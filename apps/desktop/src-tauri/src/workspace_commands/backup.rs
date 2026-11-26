use std::path::PathBuf;
use std::sync::Arc;

use dd_infra::workspace_backup::WorkspacePackageError;
use dd_infra::workspace_backup::task::{BackupService, BackupSubmission, BackupTaskSnapshot};
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::workspace::{
    WorkspaceBackupSubmitRequest, WorkspaceBackupView, WorkspaceRestoreRequest,
};

use dd_core::workspace::WorkspaceAccessError;
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::UuidText;
use dd_ui_contracts::workspace::{AppError, CommandContext, PathGrant, WorkspaceSnapshot};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::{Commands, GRANT_LIFETIME, run};

#[tauri::command]
pub async fn workspace_restore(
    request: WorkspaceRestoreRequest,
    app: AppHandle,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<Option<WorkspaceSnapshot>, AppError> {
    run(thread, request.request_id, move |commands| {
        commands.validate_restore_folder_name(&request.folder_name)?;
        if commands
            .thread
            .current()
            .map_err(|failure| commands.map_error(failure))?
            .is_some()
        {
            return Err(commands.error("FAILED_PRECONDITION", "请先关闭当前工作区。"));
        }
        let Some(package) = app
            .dialog()
            .file()
            .set_title("选择工作区备份包目录")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let Some(parent) = app
            .dialog()
            .file()
            .set_title("选择恢复位置的上级目录")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let local_path = |selected: tauri_plugin_dialog::FilePath| {
            selected
                .into_path()
                .map_err(|_| commands.error("INVALID_ARGUMENT", "请选择本机目录。"))
        };
        commands
            .restore_backup(
                local_path(package)?,
                local_path(parent)?,
                request.folder_name,
            )
            .map(Some)
    })
    .await
}

impl Commands {
    fn validate_restore_folder_name(&self, name: &str) -> Result<(), AppError> {
        let base = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let device_name = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
            .contains(&base.as_str())
            || ["COM", "LPT"].iter().any(|prefix| {
                base.strip_prefix(prefix).is_some_and(|number| {
                    matches!(
                        number,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            });
        if name.is_empty()
            || name.chars().count() > 128
            || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
            || name.ends_with(['.', ' '])
            || name.chars().any(char::is_control)
            || device_name
        {
            return Err(self.error("INVALID_ARGUMENT", "请输入有效的新工作区目录名称。"));
        }
        Ok(())
    }

    pub(super) fn restore_backup(
        &self,
        package: PathBuf,
        parent: PathBuf,
        folder_name: String,
    ) -> Result<WorkspaceSnapshot, AppError> {
        self.validate_restore_folder_name(&folder_name)?;
        let parent = parent
            .canonicalize()
            .map_err(|_| self.error("PERMISSION_DENIED", "无法读取所选恢复位置。"))?;
        let restored = self
            .thread
            .restore_backup(package, parent.join(folder_name))
            .map_err(|failure| match failure {
                WorkspaceThreadError::Access(WorkspaceAccessError::AlreadyOpen) => {
                    self.error("FAILED_PRECONDITION", "请先关闭当前工作区。")
                }
                WorkspaceThreadError::Backup(WorkspacePackageError::DestinationExists) => {
                    self.error("CONFLICT", "目标目录已存在，请使用其他名称。")
                }
                WorkspaceThreadError::Backup(_) => self.error(
                    "FAILED_PRECONDITION",
                    "无法恢复，请检查备份的完整性和目标位置的访问权限。",
                ),
                failure => self.map_error(failure),
            })?;
        self.snapshot(restored)
    }
}

#[tauri::command]
pub async fn workspace_backup_submit(
    request: WorkspaceBackupSubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ImportOperationRef, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            commands.submit_backup(
                request.context,
                request.destination_grant_id,
                request.include_reports,
            )
        },
    )
    .await
}

#[tauri::command]
pub async fn workspace_backup_latest(
    context: CommandContext,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<Option<ImportTaskView>, AppError> {
    run(thread, context.request_id.clone(), move |commands| {
        commands.latest_backup(context)
    })
    .await
}

impl Commands {
    pub(super) fn backup_directory(
        &self,
        context: CommandContext,
        selection: PathBuf,
    ) -> Result<PathGrant, AppError> {
        let session = self.session(context)?;
        let display_path = selection.to_string_lossy().into_owned();
        let grant = self
            .thread
            .grant_backup_directory(session, selection)
            .map_err(|failure| self.map_error(failure))?;
        let expires_at = (OffsetDateTime::now_utc() + GRANT_LIFETIME)
            .format(&Rfc3339)
            .map_err(|_| self.error("INTERNAL", "无法生成目录授权。"))?;
        Ok(PathGrant {
            grant_id: self.uuid_text(grant.grant_id.to_string())?,
            display_path,
            allowed_operations: vec!["WORKSPACE_BACKUP".into()],
            expires_at,
        })
    }

    pub(super) fn submit_backup(
        &self,
        context: CommandContext,
        destination_grant_id: UuidText,
        include_reports: bool,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(context)?;
        let service = BackupService::new(Arc::clone(&self.thread));
        let (task, job) = service
            .submit(
                session,
                BackupSubmission {
                    request_id,
                    idempotency_key,
                    destination_grant_id: self.parse_uuid(destination_grant_id)?,
                    include_reports,
                },
                self.now_ms()?,
            )
            .map_err(|failure| match failure {
                WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict) => {
                    self.error("CONFLICT", "此备份请求的内容已变化，请重新选择目录。")
                }
                WorkspaceThreadError::Backup(WorkspacePackageError::InvalidDestination) => {
                    self.error("INVALID_ARGUMENT", "请选择工作区目录以外的备份位置。")
                }
                WorkspaceThreadError::Backup(WorkspacePackageError::ReportsRequired) => {
                    self.error("INVALID_ARGUMENT", "工作区备份必须包含报告。")
                }
                failure => self.map_error(failure),
            })?;
        let operation = self.import_operation(
            ImportOperationKind::WorkspaceBackup,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )?;
        if let Some(job) = job {
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = service.run_admitted(session, job, || {
                    OffsetDateTime::now_utc()
                        .unix_timestamp_nanos()
                        .div_euclid(1_000_000) as i64
                }) {
                    eprintln!("Workspace backup task {}: {error}", task.task_id);
                }
            });
        }
        Ok(operation)
    }

    pub(super) fn latest_backup(
        &self,
        context: CommandContext,
    ) -> Result<Option<ImportTaskView>, AppError> {
        let session = self.session(context)?;
        BackupService::new(Arc::clone(&self.thread))
            .latest(session, self.now_ms()?)
            .map_err(|failure| self.map_error(failure))?
            .map(|task| self.backup_task_view(task))
            .transpose()
    }

    pub(super) fn backup_task(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let task = BackupService::new(Arc::clone(&self.thread))
            .snapshot(session, task, self.now_ms()?)
            .map_err(|failure| self.map_error(failure))?
            .ok_or_else(|| self.error("NOT_FOUND", "当前工作区中没有此任务。"))?;
        self.backup_task_view(task)
    }

    fn backup_task_view(&self, task: BackupTaskSnapshot) -> Result<ImportTaskView, AppError> {
        Ok(ImportTaskView {
            color_preview: None,
            color_apply: None,
            staging_commit: None,
            video_tracking: None,
            task_id: self.uuid_text(task.task_id.to_string())?,
            operation_kind: ImportOperationKind::WorkspaceBackup,
            status: task.status,
            error_code: task.error_code,
            video_sample: None,
            video_frame: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            report_export: None,
            workspace_backup: task.receipt.map(|receipt| WorkspaceBackupView {
                package_path: receipt.package.to_string_lossy().into_owned(),
                asset_file_count: (receipt.asset_files as u64).into(),
            }),
        })
    }
}
