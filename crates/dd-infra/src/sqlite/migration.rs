use std::error::Error;
use std::fmt::{self, Display};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{WorkspaceConnectionError, initialize_workspace_write_connection};

const INITIAL_VERSION: i64 = 1;
const INITIAL_SCHEMA: &str = include_str!("../../../../migrations/workspace/0001_initial.sql");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceMigrationStage {
    InspectSchema,
    Begin,
    CreateSchema,
    InsertMetadata,
    IntegrityCheck,
    ForeignKeyCheck,
    Commit,
}

#[derive(Debug)]
pub enum WorkspaceMigrationError {
    Connection(WorkspaceConnectionError),
    Sqlite {
        stage: WorkspaceMigrationStage,
        source: rusqlite::Error,
    },
    UnrecognizedSchema,
    MissingMetadata,
    WorkspaceIdMismatch,
    UnsupportedVersion(i64),
    IntegrityCheck(String),
    ForeignKeyCheck {
        table: String,
        row_id: Option<i64>,
        parent: String,
    },
}

impl Display for WorkspaceMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(source) => write!(formatter, "Workspace write connection: {source}"),
            Self::Sqlite { stage, source } => write!(formatter, "{stage:?}: {source}"),
            Self::UnrecognizedSchema => {
                write!(formatter, "Nonempty Workspace database lacks schema_meta")
            }
            Self::MissingMetadata => {
                write!(formatter, "Workspace schema_meta lacks its singleton row")
            }
            Self::WorkspaceIdMismatch => write!(
                formatter,
                "Workspace database identity does not match its marker"
            ),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "Unsupported Workspace schema version {version}")
            }
            Self::IntegrityCheck(report) => {
                write!(formatter, "Workspace integrity_check: {report}")
            }
            Self::ForeignKeyCheck {
                table,
                row_id,
                parent,
            } => {
                write!(
                    formatter,
                    "Workspace foreign_key_check: {table} row {row_id:?} references {parent}"
                )
            }
        }
    }
}

impl Error for WorkspaceMigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Connection(source) => Some(source),
            Self::Sqlite { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Initializes a new Workspace schema or reopens a matching v1 database.
///
/// Core supplies an already opened, local Workspace connection, the validated
/// marker UUID and the current UTC time in milliseconds. The caller holds the
/// Workspace ownership lock. On failure the connection is dropped and an
/// incomplete migration is rolled back. The preceding WAL change can persist.
pub fn migrate_workspace_database(
    connection: Connection,
    busy_timeout_ms: u32,
    workspace_id: &str,
    now_ms: i64,
) -> Result<Connection, WorkspaceMigrationError> {
    use WorkspaceMigrationStage::*;

    // Reject an unrelated or unsupported database before changing its journal mode.
    schema_is_empty(&connection, workspace_id)?;
    let mut connection = initialize_workspace_write_connection(connection, busy_timeout_ms)
        .map_err(WorkspaceMigrationError::Connection)?;
    let transaction = sqlite(
        Begin,
        connection.transaction_with_behavior(TransactionBehavior::Immediate),
    )?;
    if schema_is_empty(&transaction, workspace_id)? {
        sqlite(CreateSchema, transaction.execute_batch(INITIAL_SCHEMA))?;
        sqlite(
            InsertMetadata,
            transaction.execute(
                "INSERT INTO main.schema_meta
                 (singleton, workspace_id, schema_version, created_at_ms, updated_at_ms)
                 VALUES (1, ?1, ?2, ?3, ?3)",
                params![workspace_id, INITIAL_VERSION, now_ms],
            ),
        )?;
        check_schema(&transaction)?;
    }
    sqlite(Commit, transaction.commit())?;
    Ok(connection)
}

fn schema_is_empty(
    connection: &Connection,
    workspace_id: &str,
) -> Result<bool, WorkspaceMigrationError> {
    use WorkspaceMigrationStage::InspectSchema;

    let (has_objects, has_metadata): (bool, bool) = sqlite(
        InspectSchema,
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema),
                    EXISTS(SELECT 1 FROM main.sqlite_schema
                           WHERE type = 'table' AND name = 'schema_meta')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ),
    )?;
    if !has_objects {
        return Ok(true);
    }
    if !has_metadata {
        return Err(WorkspaceMigrationError::UnrecognizedSchema);
    }
    let (stored_id, version): (String, i64) = sqlite(
        InspectSchema,
        connection
            .query_row(
                "SELECT workspace_id, schema_version FROM main.schema_meta WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional(),
    )?
    .ok_or(WorkspaceMigrationError::MissingMetadata)?;
    if !stored_id.eq_ignore_ascii_case(workspace_id) {
        return Err(WorkspaceMigrationError::WorkspaceIdMismatch);
    }
    if version != INITIAL_VERSION {
        return Err(WorkspaceMigrationError::UnsupportedVersion(version));
    }
    Ok(false)
}

fn check_schema(connection: &Connection) -> Result<(), WorkspaceMigrationError> {
    use WorkspaceMigrationStage::{ForeignKeyCheck, IntegrityCheck};

    let report: String = sqlite(
        IntegrityCheck,
        connection.query_row("PRAGMA main.integrity_check", [], |row| row.get(0)),
    )?;
    if report != "ok" {
        return Err(WorkspaceMigrationError::IntegrityCheck(report));
    }
    let violation = sqlite(
        ForeignKeyCheck,
        connection
            .query_row("PRAGMA main.foreign_key_check", [], |row| {
                Ok(WorkspaceMigrationError::ForeignKeyCheck {
                    table: row.get(0)?,
                    row_id: row.get(1)?,
                    parent: row.get(2)?,
                })
            })
            .optional(),
    )?;
    match violation {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn sqlite<T>(
    stage: WorkspaceMigrationStage,
    result: rusqlite::Result<T>,
) -> Result<T, WorkspaceMigrationError> {
    result.map_err(|source| WorkspaceMigrationError::Sqlite { stage, source })
}
