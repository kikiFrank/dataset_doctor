use std::sync::Arc;
use std::time::Duration;

use dd_core::workspace::{WorkspaceAccessError, WorkspaceSession};
use dd_infra::workspace::WorkspaceOpenError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::UuidText;
use dd_ui_contracts::workspace::{
    AppError, CloseReceipt, CloseState, CommandContext, PathGrant, PathSelectRequest,
    WorkspaceCloseRequest, WorkspaceCreateRequest, WorkspaceGetRequest, WorkspaceOpenRequest,
    WorkspaceSnapshot,
};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::RuntimeMode;

pub(crate) mod dataset;

pub const GRANT_LIFETIME: Duration = Duration::from_secs(120);

#[cfg(test)]
#[path = "../../../../tests/desktop/host/workspace_create.rs"]
mod create_tests;

#[cfg(test)]
#[path = "../../../../tests/desktop/host/dataset_browse.rs"]
mod dataset_tests;

#[tauri::command]
pub async fn path_select(
    request: PathSelectRequest,
    app: AppHandle,
    thread: State<'_, Arc<WorkspaceThread>>,
    runtime: State<'_, RuntimeMode>,
) -> Result<Option<PathGrant>, AppError> {
    let selected_fixture = runtime.smoke_workspace.clone();
    run(thread, request.request_id, move |commands| {
        let selection = if let Some(path) = selected_fixture {
            path
        } else {
            let Some(path) = app.dialog().file().blocking_pick_folder() else {
                return Ok(None);
            };
            path.into_path()
                .map_err(|_| commands.error("INVALID_ARGUMENT", "请选择本机目录。"))?
        };
        let root = selection
            .canonicalize()
            .map_err(|_| commands.error("PERMISSION_DENIED", "无法读取所选目录。"))?;
        if !root.is_dir() {
            return Err(commands.error("INVALID_ARGUMENT", "请选择工作区目录。"));
        }
        let grant = commands
            .thread
            .grant_selected_directory(root)
            .map_err(|failure| commands.map_error(failure))?;
        let expires_at = (OffsetDateTime::now_utc() + GRANT_LIFETIME)
            .format(&Rfc3339)
            .map_err(|_| commands.error("INTERNAL", "无法生成目录授权。"))?;
        Ok(Some(PathGrant {
            grant_id: commands.uuid_text(grant.grant_id.to_string())?,
            display_path: selection.to_string_lossy().into_owned(),
            allowed_operations: vec!["WORKSPACE_OPEN".into(), "WORKSPACE_CREATE".into()],
            expires_at,
        }))
    })
    .await
}

#[tauri::command]
pub async fn workspace_open(
    request: WorkspaceOpenRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<WorkspaceSnapshot, AppError> {
    run(thread, request.request_id, move |commands| {
        let grant = commands.parse_uuid(request.directory_grant_id)?;
        let opened = commands
            .thread
            .open(grant)
            .map_err(|failure| commands.map_error(failure))?;
        commands.snapshot(opened)
    })
    .await
}

#[tauri::command]
pub async fn workspace_create(
    request: WorkspaceCreateRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<WorkspaceSnapshot, AppError> {
    run(thread, request.request_id.clone(), move |commands| {
        commands.create(request)
    })
    .await
}

#[tauri::command]
pub async fn workspace_get(
    request: WorkspaceGetRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<WorkspaceSnapshot, AppError> {
    run(thread, request.request_id, move |commands| {
        let current = commands
            .thread
            .current()
            .map_err(|failure| commands.map_error(failure))?;
        match (current, request.context) {
            (Some(current), Some(context)) => {
                commands.verify_context(&current, context)?;
                commands.snapshot(current)
            }
            (None, None) => Ok(WorkspaceSnapshot::Closed),
            (None, Some(_)) => Err(commands.error("WORKSPACE_NOT_OPEN", "工作区已关闭。")),
            (Some(_), None) => {
                Err(commands.error("STALE_WORKSPACE_SESSION", "请提供当前工作区会话。"))
            }
        }
    })
    .await
}

#[tauri::command]
pub async fn workspace_close(
    request: WorkspaceCloseRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<CloseReceipt, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            let expected = commands.parse_uuid(request.expected_session_id)?;
            let current = commands
                .thread
                .current()
                .map_err(|failure| commands.map_error(failure))?
                .ok_or_else(|| commands.error("WORKSPACE_NOT_OPEN", "工作区已关闭。"))?;
            commands.verify_context(&current, request.context)?;
            commands
                .thread
                .close(expected)
                .map_err(|failure| commands.map_error(failure))?;
            Ok(CloseReceipt {
                workspace_id: commands.uuid_text(current.workspace_id)?,
                workspace_session_id: commands.uuid_text(expected.to_string())?,
                close_state: CloseState::Closed,
                blocking_task_ids: Vec::new(),
            })
        },
    )
    .await
}

async fn run<T: Send + 'static>(
    thread: State<'_, Arc<WorkspaceThread>>,
    request_id: UuidText,
    action: impl FnOnce(Commands) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let commands = Commands {
        thread: Arc::clone(thread.inner()),
        request_id: request_id.clone(),
    };
    tauri::async_runtime::spawn_blocking(move || action(commands))
        .await
        .map_err(|_| AppError {
            code: "INTERNAL".into(),
            message: "桌面命令未完成。".into(),
            request_id,
            retryable: false,
        })?
}

struct Commands {
    thread: Arc<WorkspaceThread>,
    request_id: UuidText,
}

impl Commands {
    fn session(&self, context: CommandContext) -> Result<Uuid, AppError> {
        let current = self
            .thread
            .current()
            .map_err(|failure| self.map_error(failure))?
            .ok_or_else(|| self.error("WORKSPACE_NOT_OPEN", "工作区已关闭。"))?;
        self.verify_context(&current, context)?;
        Ok(current.workspace_session_id)
    }

    fn create(&self, request: WorkspaceCreateRequest) -> Result<WorkspaceSnapshot, AppError> {
        let grant = self.parse_uuid(request.directory_grant_id)?;
        let now_ms = i64::try_from(
            OffsetDateTime::now_utc()
                .unix_timestamp_nanos()
                .div_euclid(1_000_000),
        )
        .map_err(|_| self.error("INTERNAL", "无法获取创建时间。"))?;
        let created = self
            .thread
            .create(grant, request.display_name, now_ms)
            .map_err(|failure| match failure {
                WorkspaceThreadError::Access(WorkspaceAccessError::Storage(
                    WorkspaceOpenError::ExistingWorkspace,
                )) => self.error(
                    "FAILED_PRECONDITION",
                    "此目录已有工作区，请使用打开工作区。",
                ),
                WorkspaceThreadError::Access(WorkspaceAccessError::Storage(
                    WorkspaceOpenError::Marker(_),
                )) => self.error("INVALID_ARGUMENT", "请输入 1 至 128 个字符的工作区名称。"),
                WorkspaceThreadError::Access(WorkspaceAccessError::Storage(
                    WorkspaceOpenError::PendingNameMismatch,
                )) => self.error(
                    "FAILED_PRECONDITION",
                    "此目录有未完成的创建，请使用原工作区名称重试。",
                ),
                WorkspaceThreadError::Access(WorkspaceAccessError::Storage(
                    WorkspaceOpenError::UnownedDatabase,
                )) => self.error(
                    "FAILED_PRECONDITION",
                    "目录包含未归属的数据库文件，请选择其他目录。",
                ),
                WorkspaceThreadError::Access(WorkspaceAccessError::Storage(_)) => self.error(
                    "FAILED_PRECONDITION",
                    "无法创建工作区，请检查目录的访问权限和使用状态。",
                ),
                failure => self.map_error(failure),
            })?;
        self.snapshot(created)
    }

    fn verify_context(
        &self,
        current: &WorkspaceSession,
        context: CommandContext,
    ) -> Result<(), AppError> {
        let identity = String::from(context.workspace_id);
        let session = self.parse_uuid(context.workspace_session_id)?;
        if !identity.eq_ignore_ascii_case(&current.workspace_id)
            || session != current.workspace_session_id
        {
            return Err(self.error("STALE_WORKSPACE_SESSION", "工作区会话已更新。"));
        }
        Ok(())
    }

    fn snapshot(&self, current: WorkspaceSession) -> Result<WorkspaceSnapshot, AppError> {
        Ok(WorkspaceSnapshot::Open {
            workspace_id: self.uuid_text(current.workspace_id)?,
            workspace_session_id: self.uuid_text(current.workspace_session_id.to_string())?,
            display_name: current.display_name,
            config_revision: current.config_revision.into(),
            active_task_count: 0.into(),
            offline_asset_count: current.offline_asset_count.into(),
            capabilities: Vec::new(),
        })
    }

    fn parse_uuid(&self, value: UuidText) -> Result<Uuid, AppError> {
        Uuid::parse_str(&String::from(value))
            .map_err(|_| self.error("INVALID_ARGUMENT", "标识格式不正确。"))
    }

    fn uuid_text(&self, value: String) -> Result<UuidText, AppError> {
        UuidText::try_from(value).map_err(|_| self.error("INTERNAL", "无法表示标识。"))
    }

    fn map_error(&self, failure: WorkspaceThreadError) -> AppError {
        let (code, message) = match failure {
            WorkspaceThreadError::Access(
                WorkspaceAccessError::NotAuthorized | WorkspaceAccessError::ExpiredGrant,
            ) => ("PERMISSION_DENIED", "目录授权已失效，请重新选择。"),
            WorkspaceThreadError::Access(WorkspaceAccessError::AlreadyOpen) => {
                ("RESOURCE_BUSY", "请先关闭当前工作区。")
            }
            WorkspaceThreadError::Access(WorkspaceAccessError::StaleSession) => {
                ("STALE_WORKSPACE_SESSION", "工作区会话已更新。")
            }
            WorkspaceThreadError::Access(WorkspaceAccessError::Closed) => {
                ("WORKSPACE_NOT_OPEN", "工作区已关闭。")
            }
            WorkspaceThreadError::Access(WorkspaceAccessError::InvalidSelection) => {
                ("INVALID_ARGUMENT", "请选择工作区目录。")
            }
            WorkspaceThreadError::Access(WorkspaceAccessError::Storage(_)) => (
                "INVALID_ARGUMENT",
                "无法打开工作区，请核对标志文件、数据库身份和使用状态。",
            ),
            WorkspaceThreadError::Busy => ("RESOURCE_BUSY", "工作区请求队列已满。"),
            _ => ("INTERNAL", "工作区服务不可用。"),
        };
        self.error(code, message)
    }

    fn error(&self, code: &str, message: &str) -> AppError {
        AppError {
            code: code.into(),
            message: message.into(),
            request_id: self.request_id.clone(),
            retryable: code == "RESOURCE_BUSY",
        }
    }
}
