//! Persistence for Core-authorized task submissions in the current Workspace.

use std::error::Error;
use std::fmt::{self, Display, Write};
use std::path::PathBuf;

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
    IdempotencyConflict,
    InvalidManifestPath,
    Database(rusqlite::Error),
}

impl Display for TaskStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

fn format_digest(digest: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a String does not fail");
    }
    encoded
}
