use std::error::Error;
use std::fmt::{self, Display};
use std::time::Duration;

use rusqlite::Connection;

/// The input check or connection setting at which initialization failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceConnectionStage {
    Input,
    ForeignKeys,
    BusyTimeout,
    JournalMode,
    Synchronous,
}

/// Why a Workspace write connection could not be initialized.
#[derive(Debug)]
pub enum WorkspaceConnectionError {
    NotFileBacked,
    ReadOnlyMain,
    ActiveTransaction,
    InvalidBusyTimeout {
        requested_ms: u32,
    },
    Sqlite {
        stage: WorkspaceConnectionStage,
        source: rusqlite::Error,
    },
    UnexpectedSetting {
        stage: WorkspaceConnectionStage,
        expected: String,
        actual: String,
    },
}

impl WorkspaceConnectionError {
    pub fn stage(&self) -> WorkspaceConnectionStage {
        match self {
            Self::NotFileBacked
            | Self::ReadOnlyMain
            | Self::ActiveTransaction
            | Self::InvalidBusyTimeout { .. } => WorkspaceConnectionStage::Input,
            Self::Sqlite { stage, .. } | Self::UnexpectedSetting { stage, .. } => *stage,
        }
    }
}

impl Display for WorkspaceConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFileBacked => write!(formatter, "Workspace main database must be file-backed"),
            Self::ReadOnlyMain => write!(formatter, "Workspace main database is read-only"),
            Self::ActiveTransaction => write!(
                formatter,
                "Workspace write connection must be in autocommit mode"
            ),
            Self::InvalidBusyTimeout { requested_ms } => write!(
                formatter,
                "Workspace busy timeout must be in 1..={} ms, got {requested_ms}",
                i32::MAX
            ),
            Self::Sqlite { stage, source } => write!(formatter, "{stage:?}: {source}"),
            Self::UnexpectedSetting {
                stage,
                expected,
                actual,
            } => write!(formatter, "{stage:?}: expected {expected}, got {actual}"),
        }
    }
}

impl Error for WorkspaceConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Configures a connection already opened for a local Workspace's main database.
///
/// Invalid inputs return before any PRAGMA. On error the handle is dropped. A
/// successful switch to WAL can persist on disk if a later setting fails.
pub fn initialize_workspace_write_connection(
    connection: Connection,
    busy_timeout_ms: u32,
) -> Result<Connection, WorkspaceConnectionError> {
    use WorkspaceConnectionStage::*;

    if busy_timeout_ms == 0 || busy_timeout_ms > i32::MAX as u32 {
        return Err(WorkspaceConnectionError::InvalidBusyTimeout {
            requested_ms: busy_timeout_ms,
        });
    }
    if !connection.is_autocommit() {
        return Err(WorkspaceConnectionError::ActiveTransaction);
    }
    // SQLite reports an empty main filename for temporary and in-memory databases.
    if connection.path().is_none_or(str::is_empty) {
        return Err(WorkspaceConnectionError::NotFileBacked);
    }
    if sqlite(Input, connection.is_readonly("main"))? {
        return Err(WorkspaceConnectionError::ReadOnlyMain);
    }

    sqlite(
        ForeignKeys,
        connection.pragma_update(None, "foreign_keys", true),
    )?;
    sqlite(
        BusyTimeout,
        connection.busy_timeout(Duration::from_millis(u64::from(busy_timeout_ms))),
    )?;
    let journal: String = sqlite(
        JournalMode,
        connection.query_row("PRAGMA main.journal_mode=WAL", [], |row| row.get(0)),
    )?;
    require_setting(JournalMode, "wal", journal.as_str())?;
    sqlite(
        Synchronous,
        connection.pragma_update(Some("main"), "synchronous", "FULL"),
    )?;

    let foreign_keys: i64 = sqlite(
        ForeignKeys,
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0)),
    )?;
    require_setting(ForeignKeys, 1, foreign_keys)?;
    let timeout: i64 = sqlite(
        BusyTimeout,
        connection.pragma_query_value(None, "busy_timeout", |row| row.get(0)),
    )?;
    require_setting(BusyTimeout, i64::from(busy_timeout_ms), timeout)?;
    let journal: String = sqlite(
        JournalMode,
        connection.pragma_query_value(Some("main"), "journal_mode", |row| row.get(0)),
    )?;
    require_setting(JournalMode, "wal", journal.as_str())?;
    let synchronous: i64 = sqlite(
        Synchronous,
        connection.pragma_query_value(Some("main"), "synchronous", |row| row.get(0)),
    )?;
    require_setting(Synchronous, 2, synchronous)?;

    Ok(connection)
}

fn sqlite<T>(
    stage: WorkspaceConnectionStage,
    result: rusqlite::Result<T>,
) -> Result<T, WorkspaceConnectionError> {
    result.map_err(|source| WorkspaceConnectionError::Sqlite { stage, source })
}

fn require_setting<T: PartialEq + Display>(
    stage: WorkspaceConnectionStage,
    expected: T,
    actual: T,
) -> Result<(), WorkspaceConnectionError> {
    if expected != actual {
        return Err(WorkspaceConnectionError::UnexpectedSetting {
            stage,
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}
