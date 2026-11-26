use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DecimalU64, UuidText};

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PathPurpose {
    Workspace,
    Import,
    Export,
    Backup,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PathKind {
    Directory,
    File,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PathAccess {
    Read,
    ReadWrite,
}

#[derive(Deserialize, TS)]
pub struct PathSelectRequest {
    pub request_id: UuidText,
    pub purpose: PathPurpose,
    pub kind: PathKind,
    pub access: PathAccess,
    #[serde(default)]
    #[ts(optional)]
    pub context: Option<CommandContext>,
}

#[derive(Serialize, TS)]
pub struct PathGrant {
    pub grant_id: UuidText,
    pub display_path: String,
    pub allowed_operations: Vec<String>,
    pub expires_at: String,
}

#[derive(Deserialize, TS)]
pub struct WorkspaceOpenRequest {
    pub request_id: UuidText,
    pub directory_grant_id: UuidText,
}

#[derive(Deserialize, TS)]
pub struct WorkspaceCreateRequest {
    pub request_id: UuidText,
    pub directory_grant_id: UuidText,
    pub display_name: String,
}

#[derive(Deserialize, TS)]
pub struct WorkspaceRestoreRequest {
    pub request_id: UuidText,
    pub folder_name: String,
}

#[derive(Deserialize, TS)]
pub struct WorkspaceBackupSubmitRequest {
    pub context: CommandContext,
    pub destination_grant_id: UuidText,
    pub include_reports: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct WorkspaceBackupView {
    pub package_path: String,
    pub asset_file_count: DecimalU64,
}

#[derive(Clone, Deserialize, Serialize, TS)]
pub struct CommandContext {
    pub request_id: UuidText,
    pub workspace_id: UuidText,
    pub workspace_session_id: UuidText,
    #[serde(default)]
    #[ts(optional)]
    pub idempotency_key: Option<UuidText>,
}

#[derive(Deserialize, TS)]
pub struct WorkspaceGetRequest {
    pub request_id: UuidText,
    #[serde(default)]
    #[ts(optional)]
    pub context: Option<CommandContext>,
}

#[derive(Serialize, TS)]
#[serde(tag = "status", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkspaceSnapshot {
    Closed,
    Open {
        workspace_id: UuidText,
        workspace_session_id: UuidText,
        display_name: String,
        config_revision: DecimalU64,
        active_task_count: DecimalU64,
        offline_asset_count: DecimalU64,
        capabilities: Vec<String>,
    },
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CloseMode {
    Wait,
    Cancel,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CloseState {
    Closed,
}

#[derive(Deserialize, TS)]
pub struct WorkspaceCloseRequest {
    pub context: CommandContext,
    pub mode: CloseMode,
    pub expected_session_id: UuidText,
}

#[derive(Serialize, TS)]
pub struct CloseReceipt {
    pub workspace_id: UuidText,
    pub workspace_session_id: UuidText,
    pub close_state: CloseState,
    pub blocking_task_ids: Vec<UuidText>,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub request_id: UuidText,
    pub retryable: bool,
}
