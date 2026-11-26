use std::path::PathBuf;
use std::sync::Arc;

use dd_infra::dataset_import::{
    DatasetImportCoordinator, DatasetImportError, ImportConfirmation, ImportInspection,
};
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::dataset_import::{
    DatasetImportExternalRequest, DatasetImportInspectRequest, DatasetImportPreview,
    DatasetImportRegistration, ImportFilePreview, ImportOperationKind, ImportOperationRef,
    ImportTaskView, TaskGetRequest,
};
use dd_ui_contracts::workspace::{AppError, CommandContext, PathGrant};
use tauri::State;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::{Commands, GRANT_LIFETIME, run};

#[tauri::command]
pub async fn dataset_import_inspect(
    request: DatasetImportInspectRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    imports: State<'_, Arc<DatasetImportCoordinator>>,
) -> Result<ImportOperationRef, AppError> {
    let imports = Arc::clone(imports.inner());
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.start_import_inspection(imports, request),
    )
    .await
}

#[tauri::command]
pub async fn dataset_import_external(
    request: DatasetImportExternalRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    imports: State<'_, Arc<DatasetImportCoordinator>>,
) -> Result<ImportOperationRef, AppError> {
    let imports = Arc::clone(imports.inner());
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.start_import_confirmation(imports, request),
    )
    .await
}

#[tauri::command]
pub async fn task_get(
    request: TaskGetRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    imports: State<'_, Arc<DatasetImportCoordinator>>,
) -> Result<ImportTaskView, AppError> {
    let imports = Arc::clone(imports.inner());
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.import_task(&imports, request),
    )
    .await
}

impl Commands {
    pub(super) fn import_directory(
        &self,
        context: CommandContext,
        path: PathBuf,
    ) -> Result<PathGrant, AppError> {
        let session = self.session(context)?;
        let root = path
            .canonicalize()
            .map_err(|_| self.error("PERMISSION_DENIED", "无法读取所选目录。"))?;
        if !root.is_dir() {
            return Err(self.error("INVALID_ARGUMENT", "请选择图片目录。"));
        }
        let grant = self
            .thread
            .grant_import_directory(session, root)
            .map_err(|error| self.map_error(error))?;
        let expires_at = (OffsetDateTime::now_utc() + GRANT_LIFETIME)
            .format(&Rfc3339)
            .map_err(|_| self.error("INTERNAL", "无法生成目录授权。"))?;
        Ok(PathGrant {
            grant_id: self.uuid_text(grant.grant_id.to_string())?,
            display_path: path.to_string_lossy().into_owned(),
            allowed_operations: vec!["DATASET_IMPORT_INSPECT".into()],
            expires_at,
        })
    }

    pub(super) fn start_import_inspection(
        &self,
        imports: Arc<DatasetImportCoordinator>,
        request: DatasetImportInspectRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let dataset_name = request.dataset_name.trim().to_owned();
        if dataset_name.is_empty() || dataset_name.chars().count() > 128 {
            return Err(self.error("INVALID_ARGUMENT", "数据集名称须为 1 至 128 个字符。"));
        }
        let request = ImportInspection {
            directory_grant_id: self.parse_uuid(request.root_grant_id)?,
            dataset_name,
            request_id,
            idempotency_key,
        };
        let now = self.now_ms()?;
        let task = imports
            .submit_inspection(session, request.clone(), now)
            .map_err(|error| self.import_error(error))?;
        if task.status == "QUEUED" {
            tauri::async_runtime::spawn_blocking(move || {
                imports.run_inspection(session, request, execution_time(now))
            });
        }
        self.import_operation(
            ImportOperationKind::DatasetImportInspect,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )
    }

    pub(super) fn start_import_confirmation(
        &self,
        imports: Arc<DatasetImportCoordinator>,
        request: DatasetImportExternalRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let plan_fingerprint = decode_fingerprint(&request.plan_fingerprint)
            .ok_or_else(|| self.error("INVALID_ARGUMENT", "导入预览摘要无效。"))?;
        let request = ImportConfirmation {
            plan_id: self.parse_uuid(request.import_plan_ref)?,
            plan_fingerprint,
            request_id,
            idempotency_key,
        };
        let now = self.now_ms()?;
        let task = imports
            .submit_confirmation(session, request, now)
            .map_err(|error| self.import_error(error))?;
        if task.status == "QUEUED" {
            tauri::async_runtime::spawn_blocking(move || {
                imports.run_confirmation(session, request, execution_time(now))
            });
        }
        self.import_operation(
            ImportOperationKind::DatasetImportExternal,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )
    }

    pub(super) fn import_task(
        &self,
        imports: &DatasetImportCoordinator,
        request: TaskGetRequest,
    ) -> Result<ImportTaskView, AppError> {
        let session = self.session(request.context)?;
        let id = self.parse_uuid(request.task_id)?;
        let now = self.now_ms()?;
        match imports.inspection_task(session, id, now) {
            Ok(task) => {
                let preview = task
                    .preview
                    .map(|preview| -> Result<DatasetImportPreview, AppError> {
                        Ok(DatasetImportPreview {
                            plan_id: self.uuid_text(preview.plan_id)?,
                            plan_fingerprint: preview.plan_fingerprint,
                            dataset_name: preview.dataset_name,
                            category_names: preview.category_names,
                            can_confirm: preview.can_confirm,
                            files: preview
                                .files
                                .into_iter()
                                .map(|item| ImportFilePreview {
                                    image_path: item.image_path,
                                    media_sha256: item.media_sha256,
                                    annotation_state: item.annotation_state,
                                    annotation_path: item.annotation_path,
                                    annotation_sha256: item.annotation_sha256,
                                    issue_code: item.issue_code,
                                })
                                .collect(),
                        })
                    })
                    .transpose()?;
                Ok(ImportTaskView {
                    task_id: self.uuid_text(id.to_string())?,
                    operation_kind: ImportOperationKind::DatasetImportInspect,
                    status: task.status,
                    error_code: task.error_code,
                    registration: None,
                    preview,
                    geometry_report: None,
                    coco_export: None,
                    workspace_backup: None,
                })
            }
            Err(DatasetImportError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::UnknownTask,
            ))) => {
                let task = match imports.confirmation_task(session, id, now) {
                    Ok(task) => task,
                    Err(DatasetImportError::Workspace(WorkspaceThreadError::Task(
                        TaskStoreError::UnknownTask,
                    ))) => {
                        let queries = [
                            Self::geometry_task,
                            Self::review_commit_task,
                            Self::export_task,
                            Self::video_task,
                            Self::backup_task,
                        ];
                        for query in queries {
                            match query(self, session, id) {
                                Err(error) if error.code == "NOT_FOUND" => {}
                                result => return result,
                            }
                        }
                        return Err(self.error("NOT_FOUND", "当前工作区中没有此任务。"));
                    }
                    Err(error) => return Err(self.import_error(error)),
                };
                let registration = task
                    .registration
                    .map(|value| -> Result<DatasetImportRegistration, AppError> {
                        let count = u64::try_from(value.asset_count)
                            .map_err(|_| self.error("INTERNAL", "无法读取导入数量。"))?;
                        Ok(DatasetImportRegistration {
                            dataset_id: self.uuid_text(value.dataset_id)?,
                            asset_count: count.into(),
                        })
                    })
                    .transpose()?;
                Ok(ImportTaskView {
                    task_id: self.uuid_text(id.to_string())?,
                    operation_kind: ImportOperationKind::DatasetImportExternal,
                    status: task.status,
                    error_code: task.error_code,
                    registration,
                    preview: None,
                    geometry_report: None,
                    coco_export: None,
                    workspace_backup: None,
                })
            }
            Err(error) => Err(self.import_error(error)),
        }
    }

    pub(super) fn import_identity(
        &self,
        context: CommandContext,
    ) -> Result<(Uuid, Uuid, Uuid), AppError> {
        let request_id = self.parse_uuid(context.request_id.clone())?;
        let key = context
            .idempotency_key
            .clone()
            .ok_or_else(|| self.error("INVALID_ARGUMENT", "请求缺少幂等键。"))?;
        Ok((self.session(context)?, request_id, self.parse_uuid(key)?))
    }

    pub(super) fn import_operation(
        &self,
        operation_kind: ImportOperationKind,
        id: Uuid,
        status: String,
        accepted_at_ms: i64,
    ) -> Result<ImportOperationRef, AppError> {
        let accepted_at =
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(accepted_at_ms) * 1_000_000)
                .ok()
                .and_then(|time| time.format(&Rfc3339).ok())
                .ok_or_else(|| self.error("INTERNAL", "无法读取任务受理时间。"))?;
        Ok(ImportOperationRef {
            operation_kind,
            task_id: self.uuid_text(id.to_string())?,
            status,
            accepted_at,
        })
    }

    fn import_error(&self, error: DatasetImportError) -> AppError {
        match error {
            DatasetImportError::Approval(_) => {
                self.error("FAILED_PRECONDITION", "导入预览不可用于确认，请重新预检。")
            }
            DatasetImportError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "此幂等键已用于不同请求，或该预览已有确认任务。"),
            DatasetImportError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::UnknownTask,
            )) => self.error("NOT_FOUND", "当前工作区中没有此任务。"),
            DatasetImportError::Workspace(failure) => self.map_error(failure),
            _ => self.error("INTERNAL", "无法受理导入任务。"),
        }
    }
}

fn execution_time(fallback: i64) -> i64 {
    i64::try_from(
        OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .div_euclid(1_000_000),
    )
    .unwrap_or(fallback)
}

fn decode_fingerprint(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}
