//! Filesystem ownership of a Core-approved Workspace directory.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

const FILE_NAME: &str = ".datasetdoctor.lock";

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
