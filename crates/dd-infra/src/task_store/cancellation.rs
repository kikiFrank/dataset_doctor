use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::TaskStoreError;
use crate::workspace::OpenedWorkspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskCancellationReceipt {
    pub task_id: Uuid,
    pub requested: bool,
    pub observed_status: String,
    pub revision: u64,
}

impl OpenedWorkspace {
    /// Serialize cancellation with dispatch and terminal acceptance. Repeated
    /// requests observe the same revision until an executor confirms its stop.
    pub(crate) fn cancel_task(
        &mut self,
        task: Uuid,
        now_ms: i64,
    ) -> Result<TaskCancellationReceipt, TaskStoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, revision, capability): (String, i64, String) = tx
            .query_row(
                "SELECT status,row_revision,capability_id FROM tasks WHERE task_id=?1",
                [task.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(TaskStoreError::UnknownTask)?;
        let next = match status.as_str() {
            "QUEUED" => Some("CANCELLED"),
            "DISPATCHING" | "RUNNING"
                if matches!(
                    capability.as_str(),
                    "annotation_geometry.v1"
                        | "dataset_profile.v1"
                        | "image_quality.v1"
                        | "color_preview.v1"
                        | "video_probe.v1"
                        | "frame_resolve.v1"
                        | "video_tracking.v1"
                ) =>
            {
                Some("CANCELLING")
            }
            "DISPATCHING" | "RUNNING" => return Err(TaskStoreError::AttemptConflict),
            _ => None,
        };
        if let Some(next) = next {
            tx.execute("UPDATE tasks SET status=?2,updated_at_ms=?3,row_revision=row_revision+1,
                finished_at_ms=CASE WHEN ?2='CANCELLED' THEN ?3 ELSE finished_at_ms END,
                execution_metadata_json=json_set(execution_metadata_json,'$.cancel_requested',json('true'))
                WHERE task_id=?1", params![task.to_string(), next, now_ms])?;
        }
        let observed_status = next.unwrap_or(&status).to_owned();
        let receipt = TaskCancellationReceipt {
            task_id: task,
            requested: matches!(observed_status.as_str(), "CANCELLING" | "CANCELLED"),
            observed_status,
            revision: u64::try_from(revision).map_err(|_| TaskStoreError::AttemptConflict)?
                + u64::from(next.is_some()),
        };
        tx.commit()?;
        Ok(receipt)
    }
}
