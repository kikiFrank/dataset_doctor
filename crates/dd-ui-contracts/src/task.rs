use crate::{DecimalU64, UuidText, workspace::CommandContext};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TaskCancelRequest {
    pub context: CommandContext,
    pub task_id: UuidText,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CancellationTargetKind {
    Task,
}

#[derive(Debug, Serialize, TS)]
pub struct CancellationReceipt {
    pub target_kind: CancellationTargetKind,
    pub target_id: UuidText,
    pub requested: bool,
    pub observed_status: String,
    pub revision: DecimalU64,
}
