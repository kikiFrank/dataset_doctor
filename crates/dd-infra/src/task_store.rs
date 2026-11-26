//! Persistence for Core-authorized task submissions in the current Workspace.

use std::error::Error;
use std::fmt::{self, Display, Write};
use std::path::PathBuf;

use dd_core::execution_grant::{ClaimReceipt, GrantScope};
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params, types::Type};
use uuid::Uuid;

use crate::paths::is_portable_relative_file;
use crate::workspace::OpenedWorkspace;

#[derive(Clone, Debug)]
pub struct TaskSubmission {
    pub workspace_session: Uuid,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub operation_kind: String,
    pub capability_id: String,
    pub input_manifest_path: PathBuf,
    pub request_fingerprint: [u8; 32],
    pub input_fingerprint: [u8; 32],
}

#[derive(Debug, Eq, PartialEq)]
pub struct StoredTask {
    pub task_id: Uuid,
    pub request_id: Uuid,
    pub workspace_session: Uuid,
    pub status: String,
    pub created_at_ms: i64,
}

#[derive(Debug)]
pub enum TaskStoreError {
    AttemptConflict,
    IdempotencyConflict,
    InvalidManifestPath,
    Database(rusqlite::Error),
}

impl Display for TaskStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AttemptConflict => formatter.write_str("Task attempt or execution state changed"),
            Self::IdempotencyConflict => {
                formatter.write_str("Task idempotency key has a different request fingerprint")
            }
            Self::InvalidManifestPath => {
                formatter.write_str("Task manifest must be a Workspace-relative file")
            }
            Self::Database(error) => write!(formatter, "Persist Workspace task: {error}"),
        }
    }
}

impl Error for TaskStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for TaskStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl OpenedWorkspace {
    /// Write a new QUEUED task after Core has frozen its input. A replay returns
    /// the stored task and status without mutating its initial submission.
    pub fn enqueue_task(
        &mut self,
        request: &TaskSubmission,
        now_ms: i64,
    ) -> Result<StoredTask, TaskStoreError> {
        if !is_portable_relative_file(&request.input_manifest_path) {
            return Err(TaskStoreError::InvalidManifestPath);
        }
        let manifest = request
            .input_manifest_path
            .iter()
            .map(|part| part.to_str().expect("validated UTF-8 component"))
            .collect::<Vec<_>>()
            .join("/");
        let fingerprint = format_digest(&request.request_fingerprint);
        let idempotency_key = request.idempotency_key.to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((task, original)) = tx.query_row(
            "SELECT task_id, request_id, workspace_session_id, status, created_at_ms, request_fingerprint
             FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
            params![request.operation_kind, idempotency_key],
            |row| Ok((read_task(row)?, row.get::<_, String>(5)?)),
        ).optional()? {
            if original != fingerprint {
                return Err(TaskStoreError::IdempotencyConflict);
            }
            tx.commit()?;
            return Ok(task);
        }

        let task = StoredTask {
            task_id: Uuid::new_v4(),
            request_id: request.request_id,
            workspace_session: request.workspace_session,
            status: "QUEUED".into(),
            created_at_ms: now_ms,
        };
        tx.execute(
            "INSERT INTO tasks
                (task_id, workspace_session_id, capability_id, status, request_id, operation_kind,
                 idempotency_key, request_fingerprint, input_manifest_path, input_fingerprint,
                 created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                task.task_id.to_string(),
                task.workspace_session.to_string(),
                request.capability_id,
                task.status,
                task.request_id.to_string(),
                request.operation_kind,
                idempotency_key,
                fingerprint,
                manifest,
                format_digest(&request.input_fingerprint),
                now_ms
            ],
        )?;
        tx.commit()?;
        Ok(task)
    }

    /// Persist this attempt and its reservation before sending Start to the executor.
    pub fn record_dispatch(
        &mut self,
        scope: &GrantScope,
        grant: Uuid,
        node_spec_fingerprint: [u8; 32],
        now_ms: i64,
    ) -> Result<(), TaskStoreError> {
        let session = scope.workspace_session.to_string();
        let digest = format_digest(&scope.input_fingerprint);
        let metadata = serde_json::json!({
            "workspace_session_id": session,
            "executor_session_id": scope.executor_session.to_string(),
            "grant_id": grant.to_string(),
            "resource_reservation_id": scope.reservation.to_string(),
            "operation_digest": format_digest(&scope.operation_digest),
            "input_fingerprint": digest,
            "node_spec_fingerprint": format_digest(&node_spec_fingerprint),
            "grant_state": "ISSUED",
        })
        .to_string();
        let changed = self.connection.execute(
            "UPDATE tasks SET status='DISPATCHING', current_attempt_id=?1, attempt_no=attempt_no+1,
                              execution_metadata_json=?2, row_revision=row_revision+1, updated_at_ms=?3
             WHERE task_id=?4 AND workspace_session_id=?5 AND input_fingerprint=?6
                   AND status='QUEUED' AND current_attempt_id IS NULL",
            params![scope.attempt.to_string(), metadata, now_ms, scope.task.to_string(), session, digest],
        )?;
        if changed != 1 {
            return Err(TaskStoreError::AttemptConflict);
        }
        Ok(())
    }

    /// Persist accepted execution identity without announcing that computation has started.
    pub fn record_claimed(
        &mut self,
        workspace_session: Uuid,
        grant: Uuid,
        receipt: ClaimReceipt,
        now_ms: i64,
    ) -> Result<(), TaskStoreError> {
        self.record_attempt_event(
            workspace_session,
            grant,
            receipt,
            now_ms,
            AttemptEvent::Claimed,
        )
    }

    /// Accept an executor's start notification for the still-current accepted claim.
    pub fn record_running(
        &mut self,
        workspace_session: Uuid,
        grant: Uuid,
        receipt: ClaimReceipt,
        now_ms: i64,
    ) -> Result<(), TaskStoreError> {
        self.record_attempt_event(
            workspace_session,
            grant,
            receipt,
            now_ms,
            AttemptEvent::Running,
        )
    }

    /// Reconcile an abandoned session after the process supervisor has
    /// confirmed that the previous executors are stopped. Recovery never runs a task.
    pub fn recover_abandoned_session(
        &mut self,
        abandoned_session: Uuid,
        now_ms: i64,
    ) -> Result<usize, TaskStoreError> {
        self.connection.execute(
            "UPDATE tasks
             SET status='INTERRUPTED', finished_at_ms=?2, updated_at_ms=?2,
                 row_revision=row_revision+1, error_code='INTERRUPTED',
                 error_message='Execution was interrupted before a terminal result was accepted',
                 execution_metadata_json=json_set(execution_metadata_json, '$.grant_state', 'REVOKED')
             WHERE workspace_session_id=?1 AND status IN ('DISPATCHING', 'RUNNING', 'CANCELLING')",
            params![abandoned_session.to_string(), now_ms],
        ).map_err(TaskStoreError::Database)
    }

    fn record_attempt_event(
        &mut self,
        workspace_session: Uuid,
        grant: Uuid,
        receipt: ClaimReceipt,
        now_ms: i64,
        event: AttemptEvent,
    ) -> Result<(), TaskStoreError> {
        let task = receipt.task.to_string();
        let session = workspace_session.to_string();
        let attempt = receipt.attempt.to_string();
        let grant = grant.to_string();
        let execution = receipt.execution.to_string();
        let statement = match event {
            AttemptEvent::Claimed => "UPDATE tasks SET row_revision=row_revision+1, updated_at_ms=?6,
                 execution_metadata_json=json_set(execution_metadata_json, '$.grant_state', 'CLAIMED', '$.claimed_execution_id', ?5)
                 WHERE task_id=?1 AND workspace_session_id=?2 AND current_attempt_id=?3 AND status='DISPATCHING'
                       AND json_extract(execution_metadata_json, '$.grant_id')=?4
                       AND json_extract(execution_metadata_json, '$.grant_state')='ISSUED'",
            AttemptEvent::Running => "UPDATE tasks SET row_revision=row_revision+1, updated_at_ms=?6,
                 status='RUNNING', started_at_ms=?6
                 WHERE task_id=?1 AND workspace_session_id=?2 AND current_attempt_id=?3 AND status='DISPATCHING'
                       AND json_extract(execution_metadata_json, '$.grant_id')=?4
                       AND json_extract(execution_metadata_json, '$.grant_state')='CLAIMED'
                       AND json_extract(execution_metadata_json, '$.claimed_execution_id')=?5",
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            statement,
            params![task, session, attempt, grant, execution, now_ms],
        )?;
        if changed == 0 {
            let status: Option<String> = tx.query_row(
                "SELECT status FROM tasks WHERE task_id=?1 AND workspace_session_id=?2 AND current_attempt_id=?3
                   AND json_extract(execution_metadata_json, '$.grant_id')=?4
                   AND json_extract(execution_metadata_json, '$.grant_state')='CLAIMED'
                   AND json_extract(execution_metadata_json, '$.claimed_execution_id')=?5",
                params![task, session, attempt, grant, execution], |row| row.get(0),
            ).optional()?;
            match (event, status.as_deref()) {
                (AttemptEvent::Claimed, Some("DISPATCHING" | "RUNNING"))
                | (AttemptEvent::Running, Some("RUNNING")) => {}
                _ => return Err(TaskStoreError::AttemptConflict),
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum AttemptEvent {
    Claimed,
    Running,
}

fn read_task(row: &Row<'_>) -> rusqlite::Result<StoredTask> {
    Ok(StoredTask {
        task_id: row_uuid(row, 0)?,
        request_id: row_uuid(row, 1)?,
        workspace_session: row_uuid(row, 2)?,
        status: row.get(3)?,
        created_at_ms: row.get(4)?,
    })
}

fn row_uuid(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value: String = row.get(index)?;
    Uuid::parse_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}

pub(crate) fn format_digest(digest: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a String does not fail");
    }
    encoded
}
