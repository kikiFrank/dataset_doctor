use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use super::{KIND, ReviewFinalError, ReviewFinalService, database};
use crate::sqlite::row_uuid;

#[derive(Debug)]
pub struct FinalTaskSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub accepted_at_ms: i64,
}

impl ReviewFinalService {
    pub fn task_for_operation(
        &self,
        session: Uuid,
        operation: Uuid,
    ) -> Result<Option<FinalTaskSnapshot>, ReviewFinalError> {
        self.owner
            .workspace_action(session, move |owner| {
                owner
                    .connection
                    .query_row(
                        "SELECT t.task_id, t.status, t.error_code, t.created_at_ms
                 FROM file_operations f JOIN tasks t ON t.task_id=f.task_id
                 WHERE f.operation_kind=?1 AND f.operation_id=?2",
                        params![KIND, operation.to_string()],
                        decode,
                    )
                    .optional()
                    .map_err(database)
            })
            .map_err(ReviewFinalError::Storage)
    }

    pub fn task_snapshot(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<Option<FinalTaskSnapshot>, ReviewFinalError> {
        self.owner
            .workspace_action(session, move |owner| {
                owner
                    .connection
                    .query_row(
                        "SELECT task_id, status, error_code, created_at_ms FROM tasks
                 WHERE operation_kind=?1 AND task_id=?2",
                        params![KIND, task.to_string()],
                        decode,
                    )
                    .optional()
                    .map_err(database)
            })
            .map_err(ReviewFinalError::Storage)
    }

    pub fn pending_operations(&self, session: Uuid) -> Result<Vec<Uuid>, ReviewFinalError> {
        self.owner.workspace_action(session, |owner| {
            let mut statement = owner.connection.prepare(
                "SELECT operation_id FROM file_operations WHERE operation_kind=?1 AND status='PREPARED'
                 ORDER BY created_at_ms, operation_id",
            ).map_err(database)?;
            statement.query_map([KIND], |row| row_uuid(row, 0)).map_err(database)?
                .collect::<Result<Vec<_>, _>>().map_err(database)
        }).map_err(ReviewFinalError::Storage)
    }

    pub fn queue_resume(
        &self,
        session: Uuid,
        operation: Uuid,
        now_ms: i64,
    ) -> Result<(), ReviewFinalError> {
        self.owner
            .workspace_action(session, move |owner| {
                owner
                    .connection
                    .execute(
                        "UPDATE tasks SET status='QUEUED', error_code=NULL, finished_at_ms=NULL,
                     updated_at_ms=?3, row_revision=row_revision+1
                 WHERE task_id=(SELECT task_id FROM file_operations
                   WHERE operation_id=?1 AND operation_kind=?2 AND status='PREPARED')
                   AND status='INTERRUPTED'",
                        params![operation.to_string(), KIND, now_ms],
                    )
                    .map(|_| ())
                    .map_err(database)
            })
            .map_err(ReviewFinalError::Storage)
    }

    /// File replacement may already have happened. Keep the recorded intent
    /// recoverable, and let the caller see that resolution is still required.
    pub fn mark_resume_required(
        &self,
        session: Uuid,
        operation: Uuid,
        now_ms: i64,
    ) -> Result<(), ReviewFinalError> {
        self.owner
            .workspace_action(session, move |owner| {
                owner
                    .connection
                    .execute(
                        "UPDATE tasks SET status='INTERRUPTED', error_code='FINAL_RESUME_REQUIRED',
                     finished_at_ms=?3, updated_at_ms=?3, row_revision=row_revision+1
                 WHERE task_id=(SELECT task_id FROM file_operations
                   WHERE operation_id=?1 AND operation_kind=?2 AND status='PREPARED')
                   AND status IN ('QUEUED','RUNNING')",
                        params![operation.to_string(), KIND, now_ms],
                    )
                    .map(|_| ())
                    .map_err(database)
            })
            .map_err(ReviewFinalError::Storage)
    }
}

fn decode(row: &Row<'_>) -> rusqlite::Result<FinalTaskSnapshot> {
    Ok(FinalTaskSnapshot {
        task_id: row_uuid(row, 0)?,
        status: row.get(1)?,
        error_code: row.get(2)?,
        accepted_at_ms: row.get(3)?,
    })
}
