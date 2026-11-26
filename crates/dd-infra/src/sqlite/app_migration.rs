use std::error::Error;
use std::fmt::{self, Display};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

const VERSION: i64 = 1;
const SCHEMA: &str = include_str!("../../../../migrations/app/0001_initial.sql");

#[derive(Debug)]
pub enum AppMigrationError {
    InvalidConnection,
    InvalidTimeout(u32),
    UnrecognizedSchema,
    UnsupportedVersion(i64),
    IntegrityCheck(String),
    ForeignKeyViolation,
    Sqlite {
        step: &'static str,
        source: rusqlite::Error,
    },
}

impl Display for AppMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConnection => {
                write!(formatter, "app.db requires a writable file in autocommit")
            }
            Self::InvalidTimeout(ms) => write!(formatter, "Invalid app.db busy timeout {ms} ms"),
            Self::UnrecognizedSchema => write!(formatter, "Unrecognized app.db schema"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "Unsupported app.db version {version}")
            }
            Self::IntegrityCheck(report) => write!(formatter, "app.db integrity_check: {report}"),
            Self::ForeignKeyViolation => {
                write!(formatter, "app.db foreign_key_check reported a violation")
            }
            Self::Sqlite { step, source } => write!(formatter, "app.db {step}: {source}"),
        }
    }
}

impl Error for AppMigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Initializes or reopens Core's local registry and settings database.
///
/// This database's schema version is `main.user_version`. The caller owns the
/// app data directory and provides the timestamp in UTC milliseconds.
pub fn migrate_app_database(
    mut connection: Connection,
    busy_timeout_ms: u32,
    now_ms: i64,
) -> Result<Connection, AppMigrationError> {
    if !connection.is_autocommit()
        || connection.path().is_none_or(str::is_empty)
        || sqlite("read-only check", connection.is_readonly("main"))?
    {
        return Err(AppMigrationError::InvalidConnection);
    }
    if busy_timeout_ms == 0 || busy_timeout_ms > i32::MAX as u32 {
        return Err(AppMigrationError::InvalidTimeout(busy_timeout_ms));
    }
    sqlite(
        "enable foreign keys",
        connection.pragma_update(None, "foreign_keys", true),
    )?;
    let foreign_keys: i64 = sqlite(
        "verify foreign keys",
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0)),
    )?;
    if foreign_keys != 1 {
        return Err(AppMigrationError::InvalidConnection);
    }
    sqlite(
        "configure busy timeout",
        connection.busy_timeout(Duration::from_millis(u64::from(busy_timeout_ms))),
    )?;

    let transaction = sqlite(
        "begin",
        connection.transaction_with_behavior(TransactionBehavior::Immediate),
    )?;
    let version: i64 = sqlite(
        "read version",
        transaction.pragma_query_value(Some("main"), "user_version", |row| row.get(0)),
    )?;
    match version {
        0 => {
            let has_objects: bool = sqlite(
                "inspect schema",
                transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema)",
                    [],
                    |row| row.get(0),
                ),
            )?;
            if has_objects {
                return Err(AppMigrationError::UnrecognizedSchema);
            }
            sqlite("create schema", transaction.execute_batch(SCHEMA))?;
            sqlite(
                "seed settings",
                transaction.execute(
                    "INSERT INTO main.app_settings(singleton, updated_at_ms) VALUES (1, ?1)",
                    [now_ms],
                ),
            )?;
            sqlite(
                "record version",
                transaction.pragma_update(Some("main"), "user_version", VERSION),
            )?;
            check_database(&transaction)?;
        }
        VERSION => {
            let is_app_schema: bool = sqlite(
                "inspect schema",
                transaction.query_row(
                    "SELECT count(*) = 2 AND sum(name IN ('app_settings', 'workspace_registry')) = 2
                     FROM main.sqlite_schema WHERE type = 'table'",
                    [],
                    |row| row.get(0),
                ),
            )?;
            if !is_app_schema {
                return Err(AppMigrationError::UnrecognizedSchema);
            }
            let has_settings: bool = sqlite(
                "check settings",
                transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.app_settings WHERE singleton = 1)",
                    [],
                    |row| row.get(0),
                ),
            )?;
            if !has_settings {
                return Err(AppMigrationError::UnrecognizedSchema);
            }
        }
        _ => return Err(AppMigrationError::UnsupportedVersion(version)),
    }
    sqlite("commit", transaction.commit())?;
    Ok(connection)
}

fn check_database(connection: &Connection) -> Result<(), AppMigrationError> {
    let report: String = sqlite(
        "integrity_check",
        connection.query_row("PRAGMA main.integrity_check", [], |row| row.get(0)),
    )?;
    if report != "ok" {
        return Err(AppMigrationError::IntegrityCheck(report));
    }
    let mut check = sqlite(
        "foreign_key_check",
        connection.prepare("PRAGMA main.foreign_key_check"),
    )?;
    if sqlite("foreign_key_check", check.exists([]))? {
        return Err(AppMigrationError::ForeignKeyViolation);
    }
    Ok(())
}

fn sqlite<T>(step: &'static str, result: rusqlite::Result<T>) -> Result<T, AppMigrationError> {
    result.map_err(|source| AppMigrationError::Sqlite { step, source })
}
