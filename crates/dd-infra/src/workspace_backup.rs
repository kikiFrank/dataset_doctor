//! Publish an isolated Workspace package and restore its files into a new root.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backup_artifacts::{
    BackupArtifactError, BackupArtifactFile, formal_backup_inventory, stage_formal_backup_artifacts,
};
use crate::backup_assets::{
    BackupAssetError, BackupAssetFile, BackupAssetRole, stage_registered_backup_assets,
};
use crate::json::{JsonContracts, JsonProfile};
use crate::paths::{copy_and_hash, is_portable_relative_file, open_scoped_file};
use crate::sqlite::{
    WorkspaceBackupError, WorkspaceMigrationError, backup_workspace_database,
    open_existing_workspace_database, row_uuid,
};
use crate::workspace::OpenedWorkspace;

#[cfg(feature = "execution-rpc")]
pub mod task;

const DATABASE: &str = "workspace.db";
const MARKER: &str = "workspace.json";
const MANIFEST: &str = "backup-manifest.json";
const MANIFEST_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct WorkspacePackageReceipt {
    pub workspace_id: Uuid,
    pub asset_files: usize,
}

#[derive(Debug)]
pub enum WorkspacePackageError {
    DestinationExists,
    InvalidDestination,
    ReportsRequired,
    InvalidManifest,
    PendingMutation,
    DigestMismatch(PathBuf),
    Snapshot(WorkspaceBackupError),
    Assets(BackupAssetError),
    Artifacts(BackupArtifactError),
    Migration(WorkspaceMigrationError),
    Sqlite(rusqlite::Error),
    Io(io::Error),
}

impl Display for WorkspacePackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationExists => {
                formatter.write_str("The selected backup or restore target exists")
            }
            Self::InvalidDestination => {
                formatter.write_str("The backup requires a destination outside the Workspace")
            }
            Self::ReportsRequired => formatter.write_str("A Workspace backup includes its reports"),
            Self::InvalidManifest => {
                formatter.write_str("Invalid Workspace backup manifest or registration")
            }
            Self::PendingMutation => {
                formatter.write_str("A Workspace file publication still needs to be reconciled")
            }
            Self::DigestMismatch(path) => write!(
                formatter,
                "Workspace backup file changed {}",
                path.display()
            ),
            Self::Snapshot(error) => error.fmt(formatter),
            Self::Assets(error) => error.fmt(formatter),
            Self::Artifacts(error) => error.fmt(formatter),
            Self::Migration(error) => error.fmt(formatter),
            Self::Sqlite(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl Error for WorkspacePackageError {}

impl From<rusqlite::Error> for WorkspacePackageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<io::Error> for WorkspacePackageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    workspace_id: Uuid,
    marker: FileProof,
    database: FileProof,
    assets: Vec<BackupAssetFile>,
    artifacts: Vec<BackupArtifactFile>,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FileProof {
    sha256: String,
    size_bytes: u64,
}

/// Publish the database snapshot and its required live files from a paused Workspace.
pub fn create_workspace_backup(
    workspace: &OpenedWorkspace,
    destination: &Path,
) -> Result<WorkspacePackageReceipt, WorkspacePackageError> {
    create_workspace_backup_with(workspace, destination, |_, _| Ok(()))
}

fn create_workspace_backup_with(
    workspace: &OpenedWorkspace,
    destination: &Path,
    prepare_publication: impl FnOnce(
        &Path,
        &WorkspacePackageReceipt,
    ) -> Result<(), WorkspacePackageError>,
) -> Result<WorkspacePackageReceipt, WorkspacePackageError> {
    let staging = new_stage(destination)?;
    let root = staging.path().canonicalize()?;
    let workspace_id = Uuid::parse_str(&workspace.identity().workspace_id)
        .map_err(|_| WorkspacePackageError::InvalidManifest)?;
    backup_workspace_database(&workspace.connection, &root.join(DATABASE))
        .map_err(WorkspacePackageError::Snapshot)?;
    let snapshot = Connection::open_with_flags(
        root.join(DATABASE),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    require_no_pending_publication(&snapshot)?;
    let marker = copy_file(
        workspace.root(),
        Path::new(MARKER),
        &root,
        Path::new(MARKER),
        None,
    )?;
    verify_marker(&root, workspace_id)?;
    let database = proof_file(&root, Path::new(DATABASE))?;
    let assets = stage_registered_backup_assets(&snapshot, workspace.root(), &root)
        .map_err(WorkspacePackageError::Assets)?;
    let artifacts = stage_formal_backup_artifacts(&snapshot, workspace.root(), &root)
        .map_err(WorkspacePackageError::Artifacts)?;
    let manifest = Manifest {
        version: 1,
        workspace_id,
        marker,
        database,
        assets,
        artifacts,
    };
    let mut sealed = File::create_new(root.join(MANIFEST))?;
    serde_json::to_writer(&mut sealed, &manifest)
        .map_err(|_| WorkspacePackageError::InvalidManifest)?;
    sealed.flush()?;
    if sealed.metadata()?.len() > MANIFEST_LIMIT {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    sealed.sync_all()?;
    drop(sealed);
    snapshot.close().map_err(|(_, error)| error)?;
    let receipt = WorkspacePackageReceipt {
        workspace_id,
        asset_files: manifest.assets.len(),
    };
    prepare_publication(&root, &receipt)?;
    publish(staging.path(), destination)?;
    Ok(receipt)
}

/// Restore into a new path and rebase registrations without opening their old locations.
pub fn restore_workspace_backup(
    package: &Path,
    destination: &Path,
) -> Result<WorkspacePackageReceipt, WorkspacePackageError> {
    let staging = new_stage(destination)?;
    let root = staging.path().canonicalize()?;
    let package = package.canonicalize()?;
    let bytes = read_manifest(&package, Path::new(MANIFEST), MANIFEST_LIMIT)?;
    let mut manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| WorkspacePackageError::InvalidManifest)?;
    if manifest.version != 1 || manifest.workspace_id.is_nil() {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    for (file, proof) in [(MARKER, &manifest.marker), (DATABASE, &manifest.database)] {
        let path = Path::new(file);
        copy_file(&package, path, &root, path, Some(proof))?;
    }
    verify_marker(&root, manifest.workspace_id)?;
    let connection = Connection::open_with_flags(
        root.join(DATABASE),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut connection =
        open_existing_workspace_database(connection, 250, &manifest.workspace_id.to_string())
            .map_err(WorkspacePackageError::Migration)?;
    require_no_pending_publication(&connection)?;
    let files = manifest.assets.len();
    restore_assets(&mut connection, &package, &root, manifest.assets)?;
    for artifact in &manifest.artifacts {
        let path = Path::new(&artifact.workspace_path);
        if artifact.package_path != artifact.workspace_path || !path.starts_with("artifacts") {
            return Err(WorkspacePackageError::InvalidManifest);
        }
        let proof = FileProof {
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        };
        copy_file(&package, path, &root, path, Some(&proof))?;
    }
    let mut inventory = formal_backup_inventory(&connection, &root, None)
        .map_err(WorkspacePackageError::Artifacts)?;
    inventory.sort_by(|first, last| first.workspace_path.cmp(&last.workspace_path));
    manifest
        .artifacts
        .sort_by(|first, last| first.workspace_path.cmp(&last.workspace_path));
    if inventory != manifest.artifacts {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    let busy = connection.query_row("PRAGMA main.wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    })?;
    if busy != 0 {
        return Err(WorkspacePackageError::PendingMutation);
    }
    connection.close().map_err(|(_, error)| error)?;
    publish(staging.path(), destination)?;
    Ok(WorkspacePackageReceipt {
        workspace_id: manifest.workspace_id,
        asset_files: files,
    })
}

struct AssetBinding {
    id: Uuid,
    location: String,
    media: String,
    media_digest: Option<String>,
    annotation: Option<String>,
    annotation_digest: Option<String>,
}

fn restore_assets(
    database: &mut Connection,
    package: &Path,
    root: &Path,
    assets: Vec<BackupAssetFile>,
) -> Result<(), WorkspacePackageError> {
    let mut files = HashMap::with_capacity(assets.len());
    for file in assets {
        if files.insert((file.asset_id, file.role), file).is_some() {
            return Err(WorkspacePackageError::InvalidManifest);
        }
    }
    let tx = database.transaction()?;
    let bindings = {
        let mut statement = tx.prepare(
            "SELECT asset_id, location_kind, path, media_digest, annotation_path, annotation_digest
             FROM main.assets ORDER BY asset_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(AssetBinding {
                    id: row_uuid(row, 0)?,
                    location: row.get(1)?,
                    media: row.get(2)?,
                    media_digest: row.get(3)?,
                    annotation: row.get(4)?,
                    annotation_digest: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for binding in bindings {
        let invalid = || WorkspacePackageError::InvalidManifest;
        let media = files
            .remove(&(binding.id, BackupAssetRole::Media))
            .ok_or_else(invalid)?;
        let path = restore_asset_file(
            package,
            root,
            &binding.location,
            &binding.media,
            binding.media_digest.as_deref(),
            media,
        )?;
        let annotation = match binding.annotation.as_deref() {
            Some(original) => {
                let file = files
                    .remove(&(binding.id, BackupAssetRole::Annotation))
                    .ok_or_else(invalid)?;
                Some(restore_asset_file(
                    package,
                    root,
                    &binding.location,
                    original,
                    binding.annotation_digest.as_deref(),
                    file,
                )?)
            }
            None => None,
        };
        tx.execute(
            "UPDATE main.assets SET path = ?2, annotation_path = ?3, location_kind = 'WORKSPACE', availability = 'ONLINE'
             WHERE asset_id = ?1",
            params![binding.id.to_string(), path, annotation],
        )?;
    }
    if !files.is_empty() {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    tx.commit()?;
    Ok(())
}

fn restore_asset_file(
    package: &Path,
    root: &Path,
    location: &str,
    original: &str,
    expected_digest: Option<&str>,
    file: BackupAssetFile,
) -> Result<String, WorkspacePackageError> {
    let packaged = Path::new(&file.package_path);
    let container = Path::new("assets").join(file.asset_id.to_string());
    if file.source_path != Path::new(original)
        || packaged.parent() != Some(container.as_path())
        || expected_digest != Some(&file.sha256)
    {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    let destination = match location {
        "WORKSPACE" => original,
        "EXTERNAL" => &file.package_path,
        _ => return Err(WorkspacePackageError::InvalidManifest),
    };
    let proof = FileProof {
        sha256: file.sha256,
        size_bytes: file.size_bytes,
    };
    copy_file(
        package,
        packaged,
        root,
        Path::new(destination),
        Some(&proof),
    )?;
    Ok(destination.to_owned())
}

fn require_no_pending_publication(database: &Connection) -> Result<(), WorkspacePackageError> {
    let pending: bool = database.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.file_operations WHERE status IN ('PREPARED','APPLYING'))",
        [],
        |row| row.get(0),
    )?;
    if pending {
        return Err(WorkspacePackageError::PendingMutation);
    }
    Ok(())
}

fn verify_marker(root: &Path, id: Uuid) -> Result<(), WorkspacePackageError> {
    let bytes = read_manifest(root, Path::new(MARKER), 1024 * 1024)?;
    let contracts = JsonContracts::new().map_err(|_| WorkspacePackageError::InvalidManifest)?;
    let marker = contracts
        .validate(JsonProfile::WorkspaceConfig, &bytes)
        .map_err(|_| WorkspacePackageError::InvalidManifest)?;
    if marker["workspace_id"].as_str() != Some(&id.to_string()) {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    Ok(())
}

fn read_manifest(
    root: &Path,
    relative: &Path,
    limit: u64,
) -> Result<Vec<u8>, WorkspacePackageError> {
    let mut bytes = Vec::new();
    open_scoped_file(root, relative)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    Ok(bytes)
}

fn proof_file(root: &Path, relative: &Path) -> Result<FileProof, WorkspacePackageError> {
    let mut source = open_scoped_file(root, relative)?;
    let (size_bytes, sha256) = copy_and_hash(&mut source, &mut io::sink())?;
    Ok(FileProof { size_bytes, sha256 })
}

fn copy_file(
    source_root: &Path,
    source_path: &Path,
    destination_root: &Path,
    destination_path: &Path,
    expected: Option<&FileProof>,
) -> Result<FileProof, WorkspacePackageError> {
    if !is_portable_relative_file(destination_path) {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    let mut source = open_scoped_file(source_root, source_path)?;
    if let Some(expected) = expected
        && source.metadata()?.len() != expected.size_bytes
    {
        return Err(WorkspacePackageError::DigestMismatch(
            source_path.to_owned(),
        ));
    }
    let target = destination_root.join(destination_path);
    fs::create_dir_all(
        target
            .parent()
            .ok_or(WorkspacePackageError::InvalidManifest)?,
    )?;
    let mut output = File::create_new(target)?;
    let (size_bytes, sha256) = copy_and_hash(&mut source, &mut output)?;
    let proof = FileProof { size_bytes, sha256 };
    if expected.is_some_and(|expected| *expected != proof) {
        return Err(WorkspacePackageError::DigestMismatch(
            source_path.to_owned(),
        ));
    }
    output.sync_all()?;
    Ok(proof)
}

fn new_stage(destination: &Path) -> Result<tempfile::TempDir, WorkspacePackageError> {
    check_unoccupied(destination)?;
    let parent = destination
        .parent()
        .filter(|path| destination.is_absolute() && path.is_dir())
        .ok_or(WorkspacePackageError::InvalidManifest)?;
    Ok(tempfile::Builder::new()
        .prefix(".workspace-package-")
        .tempdir_in(parent)?)
}

fn check_unoccupied(destination: &Path) -> Result<(), WorkspacePackageError> {
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(WorkspacePackageError::DestinationExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn publish(staged: &Path, destination: &Path) -> Result<(), WorkspacePackageError> {
    check_unoccupied(destination)?;
    rename_package(staged, destination)?;
    Ok(())
}

#[cfg(windows)]
fn rename_package(staged: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>()
    };
    let staged = wide(staged);
    let destination = wide(destination);
    // Moving a private directory uses no replace-existing flag.
    let renamed = unsafe { MoveFileExW(staged.as_ptr(), destination.as_ptr(), 0) };
    if renamed == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn rename_package(staged: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(staged, destination)
}
