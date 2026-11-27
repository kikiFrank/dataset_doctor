use super::{Commands, run};
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::task::{CancellationReceipt, CancellationTargetKind, TaskCancelRequest};
use dd_ui_contracts::workspace::AppError;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn task_cancel(
    request: TaskCancelRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<CancellationReceipt, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.request_task_cancellation(request),
    )
    .await
}

impl Commands {
    pub(super) fn request_task_cancellation(
        &self,
        request: TaskCancelRequest,
    ) -> Result<CancellationReceipt, AppError> {
        let session = self.session(request.context)?;
        let task = self.parse_uuid(request.task_id)?;
        let receipt = self
            .thread
            .cancel_task(session, task, self.now_ms()?)
            .map_err(|error| match error {
                WorkspaceThreadError::Task(TaskStoreError::AttemptConflict) => self.error(
                    "FAILED_PRECONDITION",
                    "该任务正在 Core 提交阶段，请等待其结果。",
                ),
                error => self.map_error(error),
            })?;
        Ok(CancellationReceipt {
            target_kind: CancellationTargetKind::Task,
            target_id: self.uuid_text(receipt.task_id.to_string())?,
            requested: receipt.requested,
            observed_status: receipt.observed_status,
            revision: receipt.revision.into(),
        })
    }
}
