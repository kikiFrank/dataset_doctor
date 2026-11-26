use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File};
use std::io;
use std::path::Path;

use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};
use tempfile::Builder;

use super::{WorkspaceMigrationError, migration::check_schema};

#[derive(Debug)]
pub enum WorkspaceBackupError {
    ActiveTransaction,
    InvalidSource,
    InvalidDestination,
    DestinationExists,
    SourceBusy,
    InvalidSnapshot(WorkspaceMigrationError),
    Sqlite(rusqlite::Error),
    Io(io::Error),
}

impl Display for WorkspaceBackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveTransaction => write!(formatter, "Workspace has an active transaction"),
            Self::InvalidSource => {
                write!(formatter, "Backup source is not a file-backed Workspace")
            }
            Self::InvalidDestination => write!(formatter, "Backup destination must name a file"),
            Self::DestinationExists => write!(formatter, "Backup destination already exists"),
            Self::SourceBusy => write!(
                formatter,
                "SQLite could not complete the Workspace snapshot"
            ),
            Self::InvalidSnapshot(error) => write!(formatter, "Workspace snapshot: {error}"),
            Self::Sqlite(error) => write!(formatter, "Workspace backup SQLite: {error}"),
            Self::Io(error) => write!(formatter, "Workspace backup file: {error}"),
        }
    }
}

impl Error for WorkspaceBackupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidSnapshot(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for WorkspaceBackupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<io::Error> for WorkspaceBackupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Copies a quiescent Core connection's committed WAL view into one standalone file.
/// The caller holds the Workspace lock and owns the destination directory.
pub fn backup_workspace_database(
    source: &Connection,
    destination: &Path,
) -> Result<(), WorkspaceBackupError> {
    if !source.is_autocommit() {
        return Err(WorkspaceBackupError::ActiveTransaction);
    }
    if source.path().is_none_or(str::is_empty) {
        return Err(WorkspaceBackupError::InvalidSource);
    }
    let parent = destination
        .file_name()
        .and_then(|_| destination.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(WorkspaceBackupError::InvalidDestination)?;
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(WorkspaceBackupError::DestinationExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let staging = Builder::new()
        .prefix(".workspace-backup-")
        .tempdir_in(parent)?;
    let staged = tempfile::NamedTempFile::new_in(staging.path())?.into_temp_path();
    let mut snapshot = Connection::open(&staged)?;

    let read = source.unchecked_transaction()?;
    let has_workspace: bool = read.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema
         WHERE type = 'table' AND name = 'schema_meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_workspace {
        return Err(WorkspaceBackupError::InvalidSource);
    }
    if Backup::new(&read, &mut snapshot)?.step(-1)? != StepResult::Done {
        return Err(WorkspaceBackupError::SourceBusy);
    }
    read.commit()?;

    let journal: String =
        snapshot.query_row("PRAGMA main.journal_mode = DELETE", [], |row| row.get(0))?;
    if journal != "delete" {
        return Err(WorkspaceBackupError::SourceBusy);
    }
    check_schema(&snapshot).map_err(WorkspaceBackupError::InvalidSnapshot)?;
    snapshot.close().map_err(|(_, error)| error)?;
    File::options().write(true).open(&staged)?.sync_all()?;
    match staged.persist_noclobber(destination) {
        Ok(()) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            Err(WorkspaceBackupError::DestinationExists)
        }
        Err(error) => Err(error.error.into()),
    }
}
