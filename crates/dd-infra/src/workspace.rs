//! Filesystem ownership of a Core-approved Workspace directory.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use dd_core::workspace::{WorkspaceIdentity, WorkspaceStore};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use uuid::Uuid;

use crate::json::{ContractError, JsonContracts, JsonProfile};
use crate::sqlite::{WorkspaceMigrationError, open_existing_workspace_database};

mod create;
pub use create::create_workspace;

const FILE_NAME: &str = ".datasetdoctor.lock";
const MARKER_FILE: &str = "workspace.json";
const DATABASE_FILE: &str = "workspace.db";
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
    ExistingWorkspace,
    UnownedDatabase,
    PendingNameMismatch,
    CheckpointBusy,
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
            Self::ExistingWorkspace => write!(formatter, "Open the existing Workspace"),
            Self::UnownedDatabase => write!(
                formatter,
                "Workspace directory already contains unowned database files"
            ),
            Self::PendingNameMismatch => write!(
                formatter,
                "Workspace creation is pending under a different name"
            ),
            Self::CheckpointBusy => write!(
                formatter,
                "New Workspace database could not finish its WAL checkpoint"
            ),
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
    config_revision: u64,
}

pub struct SqliteWorkspaceStore {
    contracts: JsonContracts,
    busy_timeout_ms: u32,
}

impl SqliteWorkspaceStore {
    pub fn new(busy_timeout_ms: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            contracts: JsonContracts::new()?,
            busy_timeout_ms,
        })
    }
}

impl WorkspaceStore for SqliteWorkspaceStore {
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

    fn create(
        &self,
        authorized_root: &Path,
        display_name: &str,
        workspace_id: Uuid,
        now_ms: i64,
    ) -> Result<(WorkspaceIdentity, Self::Handle), Self::Error> {
        let workspace = create_workspace(
            authorized_root,
            &self.contracts,
            self.busy_timeout_ms,
            display_name,
            &workspace_id.to_string(),
            now_ms,
        )?;
        let identity = workspace.identity().clone();
        Ok((identity, workspace))
    }
}

#[derive(Debug)]
pub struct OpenedWorkspace {
    root: PathBuf,
    identity: WorkspaceIdentity,
    // Field drop order closes SQLite before releasing the file lock.
    pub(crate) connection: Connection,
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
    let (_, marker) = read_marker(&root.join(MARKER_FILE), MARKER_FILE, contracts)?;
    let connection = open_existing_workspace_database(
        connect(&root.join(DATABASE_FILE))?,
        busy_timeout_ms,
        &marker.workspace_id,
    )
    .map_err(WorkspaceOpenError::Migration)?;
    finish_open(root, lock, marker, connection)
}

fn read_marker(
    path: &Path,
    name: &'static str,
    contracts: &JsonContracts,
) -> Result<(Vec<u8>, MarkerIdentity), WorkspaceOpenError> {
    let mut marker_options = OpenOptions::new();
    marker_options.read(true);
    prevent_replacement(&mut marker_options);
    let marker = marker_options.open(path).map_err(WorkspaceOpenError::Io)?;
    if !ordinary_file(&std::fs::symlink_metadata(path).map_err(WorkspaceOpenError::Io)?)
        || !ordinary_file(&marker.metadata().map_err(WorkspaceOpenError::Io)?)
    {
        return Err(WorkspaceOpenError::InvalidFile(name));
    }
    let mut bytes = Vec::new();
    marker
        .take(MARKER_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(WorkspaceOpenError::Io)?;
    if bytes.len() as u64 > MARKER_LIMIT {
        return Err(WorkspaceOpenError::MarkerTooLarge);
    }
    let marker = parse_marker(contracts, &bytes)?;
    Ok((bytes, marker))
}

fn parse_marker(
    contracts: &JsonContracts,
    bytes: &[u8],
) -> Result<MarkerIdentity, WorkspaceOpenError> {
    let document = contracts
        .validate(JsonProfile::WorkspaceConfig, bytes)
        .map_err(WorkspaceOpenError::Marker)?;
    serde_json::from_value(document).map_err(WorkspaceOpenError::MarkerMapping)
}

fn connect(database: &Path) -> Result<Connection, WorkspaceOpenError> {
    if !ordinary_file(&std::fs::symlink_metadata(database).map_err(WorkspaceOpenError::Io)?) {
        return Err(WorkspaceOpenError::InvalidFile(DATABASE_FILE));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    Connection::open_with_flags(database, flags).map_err(WorkspaceOpenError::Database)
}

fn finish_open(
    root: PathBuf,
    lock: WorkspaceLock,
    marker: MarkerIdentity,
    connection: Connection,
) -> Result<OpenedWorkspace, WorkspaceOpenError> {
    let offline_asset_count = connection
        .query_row(
            "SELECT count(*) FROM main.assets WHERE availability = 'OFFLINE'",
            [],
            |row| {
                let count: i64 = row.get(0)?;
                u64::try_from(count).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, count))
            },
        )
        .map_err(WorkspaceOpenError::Database)?;
    let identity = WorkspaceIdentity {
        workspace_id: marker.workspace_id,
        display_name: marker.name,
        config_revision: marker.config_revision,
        offline_asset_count,
    };
    Ok(OpenedWorkspace {
        root,
        identity,
        connection,
        _lock: lock,
    })
}

#[cfg(windows)]
pub(crate) fn prevent_replacement(options: &mut OpenOptions) {
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
pub(crate) fn prevent_replacement(_: &mut OpenOptions) {}

#[cfg(windows)]
pub(crate) fn ordinary_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

#[cfg(not(windows))]
pub(crate) fn ordinary_file(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file()
}
