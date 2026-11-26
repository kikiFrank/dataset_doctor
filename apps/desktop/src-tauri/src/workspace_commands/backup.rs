use dd_infra::workspace_backup::WorkspacePackageError;
use dd_ui_contracts::workspace::WorkspaceRestoreRequest;

use super::*;

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
