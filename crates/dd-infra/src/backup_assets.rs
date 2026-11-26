//! Copy the media and working annotations named by a Workspace backup snapshot.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths::{
    copy_and_hash, is_portable_relative_file, open_registered_file, open_scoped_file,
};
use crate::sqlite::row_uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackupAssetRole {
    Media,
    Annotation,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupAssetFile {
    pub asset_id: Uuid,
    pub role: BackupAssetRole,
    pub source_path: PathBuf,
    pub package_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug)]
pub enum BackupAssetError {
    DestinationExists,
    InvalidRegistration(Uuid),
    SourceUnavailable {
        asset_id: Uuid,
        role: BackupAssetRole,
        source: io::Error,
    },
    SourceChanged {
        asset_id: Uuid,
        role: BackupAssetRole,
    },
    Sqlite(rusqlite::Error),
    Io(io::Error),
}

impl Display for BackupAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationExists => write!(formatter, "Backup assets already exist in staging"),
            Self::InvalidRegistration(id) => {
                write!(formatter, "Invalid backup asset binding: {id}")
            }
            Self::SourceUnavailable {
                asset_id,
                role,
                source,
            } => {
                write!(formatter, "Read backup {role:?} for {asset_id}: {source}")
            }
            Self::SourceChanged { asset_id, role } => {
                write!(
                    formatter,
                    "Backup {role:?} for {asset_id} differs from its registered digest"
                )
            }
            Self::Sqlite(error) => write!(formatter, "Read registered backup assets: {error}"),
            Self::Io(error) => write!(formatter, "Write backup assets: {error}"),
        }
    }
}

impl Error for BackupAssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceUnavailable { source, .. } | Self::Io(source) => Some(source),
            Self::Sqlite(source) => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for BackupAssetError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for BackupAssetError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

struct AssetRegistration {
    id: Uuid,
    location: String,
    path: PathBuf,
    digest: Option<String>,
    annotation: Option<PathBuf>,
    annotation_digest: Option<String>,
}

impl AssetRegistration {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row_uuid(row, 0)?,
            location: row.get(1)?,
            path: PathBuf::from(row.get::<_, String>(2)?),
            digest: row.get(3)?,
            annotation: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
            annotation_digest: row.get(5)?,
        })
    }
}

/// Stage exactly the files registered in the SQLite snapshot in a private package directory.
/// The caller holds the Workspace ownership lock and pauses application commits.
pub fn stage_registered_backup_assets(
    snapshot: &Connection,
    workspace_root: &Path,
    staging_directory: &Path,
) -> Result<Vec<BackupAssetFile>, BackupAssetError> {
    let destination = staging_directory.join("assets");
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(BackupAssetError::DestinationExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let staged = tempfile::Builder::new()
        .prefix(".backup-assets-")
        .tempdir_in(staging_directory)?;
    let mut statement = snapshot.prepare(
        "SELECT asset_id, location_kind, path, media_digest, annotation_path, annotation_digest
         FROM main.assets ORDER BY asset_id",
    )?;
    let rows = statement.query_map([], AssetRegistration::read)?;
    let mut copied = Vec::new();
    for row in rows {
        let registration = row?;
        let directory = staged.path().join(registration.id.to_string());
        fs::create_dir(&directory)?;
        copied.push(copy_asset(
            workspace_root,
            &directory,
            &registration,
            BackupAssetRole::Media,
        )?);
        if registration.annotation.is_some() {
            copied.push(copy_asset(
                workspace_root,
                &directory,
                &registration,
                BackupAssetRole::Annotation,
            )?);
        }
    }
    for file in &copied {
        let mut source = open_source(workspace_root, &file.source_path, file.asset_id, file.role)?;
        let (size, digest) = copy_and_hash(&mut source, &mut io::sink()).map_err(|source| {
            BackupAssetError::SourceUnavailable {
                asset_id: file.asset_id,
                role: file.role,
                source,
            }
        })?;
        if size != file.size_bytes || digest != file.sha256 {
            return Err(BackupAssetError::SourceChanged {
                asset_id: file.asset_id,
                role: file.role,
            });
        }
    }
    fs::rename(staged.path(), destination)?;
    Ok(copied)
}

fn copy_asset(
    workspace_root: &Path,
    directory: &Path,
    registration: &AssetRegistration,
    role: BackupAssetRole,
) -> Result<BackupAssetFile, BackupAssetError> {
    let asset_id = registration.id;
    let (path, expected_digest) = match role {
        BackupAssetRole::Media => (registration.path.as_path(), registration.digest.as_deref()),
        BackupAssetRole::Annotation => (
            registration
                .annotation
                .as_deref()
                .ok_or(BackupAssetError::InvalidRegistration(asset_id))?,
            registration.annotation_digest.as_deref(),
        ),
    };
    let location_matches = match registration.location.as_str() {
        "WORKSPACE" => is_portable_relative_file(path),
        "EXTERNAL" => path.is_absolute(),
        _ => false,
    };
    let name = path
        .file_name()
        .map(Path::new)
        .filter(|name| is_portable_relative_file(name))
        .and_then(Path::to_str)
        .filter(|_| location_matches)
        .ok_or(BackupAssetError::InvalidRegistration(asset_id))?;
    let mut source = open_source(workspace_root, path, asset_id, role)?;
    let mut file = File::create_new(directory.join(name))?;
    let (size_bytes, sha256) = copy_and_hash(&mut source, &mut file)?;
    if Some(sha256.as_str()) != expected_digest {
        return Err(BackupAssetError::SourceChanged { asset_id, role });
    }
    file.sync_all()?;
    Ok(BackupAssetFile {
        asset_id,
        role,
        source_path: path.to_owned(),
        package_path: format!("assets/{asset_id}/{name}"),
        sha256,
        size_bytes,
    })
}

fn open_source(
    workspace_root: &Path,
    path: &Path,
    asset_id: Uuid,
    role: BackupAssetRole,
) -> Result<File, BackupAssetError> {
    let file = if path.is_absolute() {
        open_registered_file(path)
    } else {
        open_scoped_file(workspace_root, path)
    };
    file.map_err(|source| BackupAssetError::SourceUnavailable {
        asset_id,
        role,
        source,
    })
}
