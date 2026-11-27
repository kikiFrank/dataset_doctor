use std::sync::Arc;

use dd_infra::review_final::{
    FinalOperationState, ReviewFinalError, ReviewFinalRequest, ReviewFinalService,
};
use dd_infra::workspace_thread::WorkspaceThread;
use dd_ui_contracts::UuidText;
use dd_ui_contracts::annotation::{ReviewCommitRequest, ReviewWriteAuthorizeRequest};
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::workspace::{AppError, CommandContext, PathGrant};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Commands, run};

#[tauri::command]
pub async fn review_write_authorize(
    request: ReviewWriteAuthorizeRequest,
    app: AppHandle,
    thread: State<'_, Arc<WorkspaceThread>>,
    runtime: State<'_, crate::RuntimeMode>,
) -> Result<Option<PathGrant>, AppError> {
    let fixture = runtime
        .smoke_business_directories
        .as_ref()
        .map(|paths| paths.0.clone());
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            commands.session(request.context.clone())?;
            let selected = if let Some(path) = fixture {
                path
            } else {
                let Some(path) = app.dialog().file().blocking_pick_folder() else {
                    return Ok(None);
                };
                path.into_path()
                    .map_err(|_| commands.error("INVALID_ARGUMENT", "请选择本机标注目录。"))?
            };
            commands
                .authorize_review_write(request.context, request.review_id, selected)
                .map(Some)
        },
    )
    .await
}

#[tauri::command]
pub async fn review_commit(
    request: ReviewCommitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ImportOperationRef, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.commit_review(request),
    )
    .await
}

impl Commands {
    pub(super) fn authorize_review_write(
        &self,
        context: CommandContext,
        review: UuidText,
        selected: std::path::PathBuf,
    ) -> Result<PathGrant, AppError> {
        let session = self.session(context)?;
        let review = self.parse_uuid(review)?;
        let grant = self
            .finals()?
            .authorize_write(session, review, selected)
            .map_err(|error| self.final_error(error))?;
        let expires_at = (OffsetDateTime::now_utc() + super::GRANT_LIFETIME)
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| self.error("INTERNAL", "无法生成标注写入授权。"))?;
        Ok(PathGrant {
            grant_id: self.uuid_text(grant.grant_id.to_string())?,
            display_path: grant.display_path.to_string_lossy().into_owned(),
            allowed_operations: vec!["REVIEW_COMMIT".into()],
            expires_at,
        })
    }

    pub(super) fn commit_review(
        &self,
        request: ReviewCommitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let finals = self.finals()?;
        let write_grant = request
            .annotation_write_grant_id
            .map(|id| self.parse_uuid(id))
            .transpose()?;
        let final_request = ReviewFinalRequest {
            review_id: self.parse_uuid(request.review_id)?,
            expected_review_revision: request.expected_review_revision.into(),
            base_image_digest: request.base_image_digest,
            base_annotation_digest: request.base_annotation_digest,
            draft_digest: request.draft_digest,
            request_id,
            idempotency_key,
            confirmed_negative: request.confirmed_negative,
        };
        let now = self.now_ms()?;
        let operation = match write_grant {
            Some(grant) => finals.prepare_with_write_grant(session, final_request, grant, now),
            None => finals.prepare(session, final_request, now),
        }
        .map_err(|error| self.final_error(error))?;
        if operation.state == FinalOperationState::Prepared {
            finals
                .queue_resume(session, operation.operation_id, self.now_ms()?)
                .map_err(|error| self.final_error(error))?;
        }
        let task = finals
            .task_for_operation(session, operation.operation_id)
            .map_err(|error| self.final_error(error))?
            .ok_or_else(|| {
                self.error(
                    "FAILED_PRECONDITION",
                    "此审核来自旧版记录，请重新读取样本。",
                )
            })?;
        let result = self.import_operation(
            ImportOperationKind::ReviewCommit,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )?;
        if operation.state == FinalOperationState::Prepared {
            tauri::async_runtime::spawn_blocking(move || {
                resolve_final(&finals, session, operation.operation_id)
            });
        }
        Ok(result)
    }

    pub(super) fn review_commit_task(
        &self,
        session: Uuid,
        task_id: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let task = self
            .finals()?
            .task_snapshot(session, task_id)
            .map_err(|error| self.final_error(error))?
            .ok_or_else(|| self.error("NOT_FOUND", "当前工作区中没有此任务。"))?;
        Ok(ImportTaskView {
            color_preview: None,
            color_apply: None,
            staging_commit: None,
            video_tracking: None,
            task_id: self.uuid_text(task_id.to_string())?,
            operation_kind: ImportOperationKind::ReviewCommit,
            status: task.status,
            error_code: task.error_code,
            video_sample: None,
            video_frame: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            report_export: None,
            workspace_backup: None,
        })
    }

    pub(super) fn resume_pending_finals(&self, session: Uuid) {
        let thread = Arc::clone(&self.thread);
        tauri::async_runtime::spawn_blocking(move || {
            if let Ok(finals) = ReviewFinalService::new(thread)
                && let Ok(pending) = finals.pending_operations(session)
            {
                for operation in pending {
                    resolve_final(&finals, session, operation);
                }
            }
        });
    }

    pub(super) fn finals(&self) -> Result<ReviewFinalService, AppError> {
        ReviewFinalService::new(Arc::clone(&self.thread)).map_err(|error| self.final_error(error))
    }

    pub(super) fn final_error(&self, error: ReviewFinalError) -> AppError {
        match error {
            ReviewFinalError::WriteAuthorizationRequired => {
                self.error("PERMISSION_DENIED", "请单独授权当前标注目录的写入。")
            }
            ReviewFinalError::FileBusy => {
                self.error("FILE_BUSY", "标注文件被占用，请关闭占用程序后重试。")
            }
            ReviewFinalError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                self.error(
                    "PERMISSION_DENIED",
                    "无法写入标注文件或创建临时文件，请检查目录和文件权限后重新授权。",
                )
            }
            ReviewFinalError::Storage(error) => self.review_error(error),
            ReviewFinalError::Approval(_) => {
                self.error("FAILED_PRECONDITION", "请明确确认空标注为负样本。")
            }
            ReviewFinalError::Conflict => {
                self.error("CONFLICT", "审核草稿、样本或已受理请求发生变化。")
            }
            _ => self.error("INTERNAL", "无法受理或读取当前审核提交。"),
        }
    }
}

fn resolve_final(finals: &ReviewFinalService, session: Uuid, operation: Uuid) {
    let timestamp = || {
        OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .div_euclid(1_000_000) as i64
    };
    if finals.resume(session, operation, timestamp()).is_err() {
        let _ = finals.mark_resume_required(session, operation, timestamp());
    }
}
