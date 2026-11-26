//! Filesystem ownership of a Core-approved Workspace directory.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use dd_core::workspace::{WorkspaceIdentity, WorkspaceStore};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::json::{ContractError, JsonContracts, JsonProfile};
use crate::sqlite::{WorkspaceMigrationError, open_existing_workspace_database};

const FILE_NAME: &str = ".datasetdoctor.lock";
const MARKER_LIMIT: u64 = 1_048_576;

#[derive(Debug)]
pub enum WorkspaceLockError {
    Busy,
    InvalidLockFile,
    Io(io::Error),
}

impl Display for WorkspaceLockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(formatter, "Workspace already has a writer"),
            Self::InvalidLockFile => write!(formatter, "Workspace lock must be a regular file"),
            Self::Io(error) => write!(formatter, "Workspace lock: {error}"),
        }
    }
}

impl Error for WorkspaceLockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Retain while a Workspace is open for writes. Closing the handle releases
/// ownership. The lock file itself persists so competing writers use one file.
#[derive(Debug)]
pub struct WorkspaceLock {
    _file: File,
}

impl WorkspaceLock {
    pub fn acquire(approved_root: &Path) -> Result<Self, WorkspaceLockError> {
        let path = approved_root.join(FILE_NAME);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        prevent_replacement(&mut options);
        let file = options.open(path).map_err(WorkspaceLockError::Io)?;
        let metadata = file.metadata().map_err(WorkspaceLockError::Io)?;
        if !ordinary_file(&metadata) {
            return Err(WorkspaceLockError::InvalidLockFile);
        }
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(WorkspaceLockError::Busy),
            Err(TryLockError::Error(error)) => Err(WorkspaceLockError::Io(error)),
        }
    }
}

#[derive(Debug)]
pub enum WorkspaceOpenError {
    Io(io::Error),
    Lock(WorkspaceLockError),
    InvalidFile(&'static str),
    MarkerTooLarge,
    Marker(ContractError),
    MarkerMapping(serde_json::Error),
    Database(rusqlite::Error),
    Migration(WorkspaceMigrationError),
}

impl Display for WorkspaceOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Workspace filesystem: {error}"),
            Self::Lock(error) => Display::fmt(error, formatter),
            Self::InvalidFile(name) => write!(formatter, "Workspace {name} must be a regular file"),
            Self::MarkerTooLarge => write!(formatter, "Workspace marker exceeds 1 MiB"),
            Self::Marker(error) => Display::fmt(error, formatter),
            Self::MarkerMapping(_) => {
                write!(formatter, "Workspace marker does not map to identity")
            }
            Self::Database(error) => write!(formatter, "Open workspace.db: {error}"),
            Self::Migration(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for WorkspaceOpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Lock(error) => Some(error),
            Self::Marker(error) => Some(error),
            Self::MarkerMapping(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Migration(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct MarkerIdentity {
    workspace_id: String,
    name: String,
}

pub struct ExistingWorkspaceStore {
    contracts: JsonContracts,
    busy_timeout_ms: u32,
}

impl ExistingWorkspaceStore {
    pub fn new(busy_timeout_ms: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            contracts: JsonContracts::new()?,
            busy_timeout_ms,
        })
    }
}

impl WorkspaceStore for ExistingWorkspaceStore {
    type Handle = OpenedWorkspace;
    type Error = WorkspaceOpenError;

    fn open(
        &self,
        authorized_root: &Path,
    ) -> Result<(WorkspaceIdentity, Self::Handle), Self::Error> {
        let workspace = open_workspace(authorized_root, &self.contracts, self.busy_timeout_ms)?;
        let identity = workspace.identity().clone();
        Ok((identity, workspace))
    }
}

#[derive(Debug)]
pub struct OpenedWorkspace {
    root: PathBuf,
    identity: WorkspaceIdentity,
    // Field drop order closes SQLite before releasing the file lock.
    _connection: Connection,
    _lock: WorkspaceLock,
}

impl OpenedWorkspace {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn identity(&self) -> &WorkspaceIdentity {
        &self.identity
    }
}

/// Opens a Workspace directory that Core has authorized.
pub fn open_workspace(
    approved_root: &Path,
    contracts: &JsonContracts,
    busy_timeout_ms: u32,
) -> Result<OpenedWorkspace, WorkspaceOpenError> {
    let root = approved_root
        .canonicalize()
        .map_err(WorkspaceOpenError::Io)?;
    let lock = WorkspaceLock::acquire(&root).map_err(WorkspaceOpenError::Lock)?;
    let mut marker_options = OpenOptions::new();
    marker_options.read(true);
    prevent_replacement(&mut marker_options);
    let marker = marker_options
        .open(root.join("workspace.json"))
        .map_err(WorkspaceOpenError::Io)?;
    if !ordinary_file(&marker.metadata().map_err(WorkspaceOpenError::Io)?) {
        return Err(WorkspaceOpenError::InvalidFile("workspace.json"));
    }
    let mut bytes = Vec::new();
    marker
        .take(MARKER_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(WorkspaceOpenError::Io)?;
    if bytes.len() as u64 > MARKER_LIMIT {
        return Err(WorkspaceOpenError::MarkerTooLarge);
    }
    let document = contracts
        .validate(JsonProfile::WorkspaceConfig, &bytes)
        .map_err(WorkspaceOpenError::Marker)?;
    let marker: MarkerIdentity =
        serde_json::from_value(document).map_err(WorkspaceOpenError::MarkerMapping)?;
    let identity = WorkspaceIdentity {
        workspace_id: marker.workspace_id,
        display_name: marker.name,
    };

    let database = root.join("workspace.db");
    if !ordinary_file(&std::fs::symlink_metadata(&database).map_err(WorkspaceOpenError::Io)?) {
        return Err(WorkspaceOpenError::InvalidFile("workspace.db"));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection =
        Connection::open_with_flags(database, flags).map_err(WorkspaceOpenError::Database)?;
    let connection =
        open_existing_workspace_database(connection, busy_timeout_ms, &identity.workspace_id)
            .map_err(WorkspaceOpenError::Migration)?;
    Ok(OpenedWorkspace {
        root,
        identity,
        _connection: connection,
        _lock: lock,
    })
}

#[cfg(windows)]
fn prevent_replacement(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    // Open the lock itself and prevent delete or rename while any writer holds it.
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(windows))]
fn prevent_replacement(_: &mut OpenOptions) {}

#[cfg(windows)]
fn ordinary_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

#[cfg(not(windows))]
fn ordinary_file(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
}
