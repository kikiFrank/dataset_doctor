//! Stage the Workspace files reached by durable task, workflow and recovery records.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::json::{ContractError, JsonContracts, JsonProfile};
use crate::paths::{copy_and_hash, is_portable_relative_file, open_scoped_file};
use crate::task_store::parse_digest;

const RESULT_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupArtifactFile {
    pub workspace_path: String,
    pub package_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug)]
pub enum BackupArtifactError {
    DestinationExists,
    InvalidPath(PathBuf),
    InvalidManifest(PathBuf),
    DigestMismatch(PathBuf),
    Setup(Box<dyn Error + Send + Sync>),
    Contract(ContractError),
    Sqlite(rusqlite::Error),
    Io(io::Error),
}

impl Display for BackupArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationExists => {
                formatter.write_str("Backup artifact staging already exists")
            }
            Self::InvalidPath(path) => write!(
                formatter,
                "Invalid Workspace artifact path {}",
                path.display()
            ),
            Self::InvalidManifest(path) => {
                write!(formatter, "Invalid backup manifest {}", path.display())
            }
            Self::DigestMismatch(path) => {
                write!(formatter, "Workspace artifact changed {}", path.display())
            }
            Self::Setup(error) => write!(formatter, "Load artifact contracts: {error}"),
            Self::Contract(error) => write!(formatter, "Read sealed Workspace artifacts: {error}"),
            Self::Sqlite(error) => write!(formatter, "Read backup artifact inventory: {error}"),
            Self::Io(error) => write!(formatter, "Copy Workspace artifact: {error}"),
        }
    }
}

impl Error for BackupArtifactError {}

impl From<rusqlite::Error> for BackupArtifactError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<io::Error> for BackupArtifactError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Copy durable internal references from the frozen Workspace snapshot.
/// The caller pauses commits and owns the private backup stage.
pub fn stage_formal_backup_artifacts(
    snapshot: &Connection,
    workspace_root: &Path,
    staging_directory: &Path,
) -> Result<Vec<BackupArtifactFile>, BackupArtifactError> {
    let destination = staging_directory.join("artifacts");
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(BackupArtifactError::DestinationExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let temp = tempfile::Builder::new()
        .prefix(".backup-artifacts-")
        .tempdir_in(staging_directory)?;
    let files = formal_backup_inventory(snapshot, workspace_root, Some(temp.path()))?;
    if !files.is_empty() {
        fs::rename(temp.path().join("artifacts"), destination)?;
    }
    Ok(files)
}

pub(crate) fn formal_backup_inventory(
    snapshot: &Connection,
    workspace_root: &Path,
    destination: Option<&Path>,
) -> Result<Vec<BackupArtifactFile>, BackupArtifactError> {
    let mut staged = ArtifactStage {
        root: workspace_root,
        directory: destination,
        files: BTreeMap::new(),
    };
    let mut references = snapshot.prepare(
        "SELECT input_manifest_path, NULL FROM main.tasks
         UNION SELECT input_manifest_path, NULL FROM main.workflow_runs
         UNION SELECT checkpoint_manifest_path, NULL FROM main.workflow_runs
         UNION SELECT result_manifest_path, NULL FROM main.workflow_runs
         UNION SELECT review_manifest_path, NULL FROM main.review_sessions
         UNION SELECT manifest_path, manifest_digest FROM main.file_operations",
    )?;
    let reference_rows = references.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        ))
    })?;
    for reference in reference_rows {
        let (path, digest) = reference?;
        if let Some(path) = path {
            staged.copy(Path::new(&path), digest.as_deref(), None)?;
        }
    }
    let mut results = snapshot.prepare(
        "SELECT output_manifest_path, task_id, current_attempt_id, input_fingerprint,
                json_extract(execution_metadata_json, '$.terminal_receipt.result_manifest_ref.sha256'),
                json_extract(execution_metadata_json, '$.terminal_receipt.result_manifest_ref.size_bytes')
         FROM main.tasks WHERE output_manifest_path IS NOT NULL ORDER BY task_id",
    )?;
    let contracts = JsonContracts::new().map_err(BackupArtifactError::Setup)?;
    for result in results.query_map([], TaskResult::read)? {
        let result = result?;
        if result.path.file_name().and_then(|name| name.to_str()) == Some("result-manifest.json") {
            stage_result(&mut staged, &contracts, result)?;
        } else {
            staged.copy(&result.path, result.sha256.as_deref(), None)?;
        }
    }
    Ok(staged.files.into_values().collect())
}

struct TaskResult {
    path: PathBuf,
    task: String,
    attempt: Option<String>,
    input_fingerprint: String,
    sha256: Option<String>,
    size: Option<String>,
}

impl TaskResult {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            path: PathBuf::from(row.get::<_, String>(0)?),
            task: row.get(1)?,
            attempt: row.get(2)?,
            input_fingerprint: row.get(3)?,
            sha256: row.get(4)?,
            size: row.get(5)?,
        })
    }
}

fn stage_result(
    stage: &mut ArtifactStage<'_>,
    contracts: &JsonContracts,
    result: TaskResult,
) -> Result<(), BackupArtifactError> {
    let invalid = || BackupArtifactError::InvalidManifest(result.path.clone());
    let attempt = result.attempt.as_deref().ok_or_else(invalid)?;
    let expected_path = PathBuf::from(format!(
        "artifacts/tasks/{}/{attempt}/result-manifest.json",
        result.task
    ));
    if result.path != expected_path
        || uuid::Uuid::parse_str(&result.task).is_err()
        || uuid::Uuid::parse_str(attempt).is_err()
        || parse_digest(&result.input_fingerprint).is_none()
    {
        return Err(invalid());
    }
    let size = result
        .size
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|size| *size <= RESULT_LIMIT)
        .ok_or_else(invalid)?;
    let digest = result
        .sha256
        .as_deref()
        .filter(|value| parse_digest(value).is_some())
        .ok_or_else(invalid)?;
    stage.copy(&result.path, Some(digest), Some(size))?;
    let mut bytes = Vec::new();
    open_scoped_file(stage.directory.unwrap_or(stage.root), &result.path)?
        .take(size + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size || format!("{:x}", Sha256::digest(&bytes)) != digest {
        return Err(BackupArtifactError::DigestMismatch(result.path));
    }
    let manifest = contracts
        .validate(JsonProfile::TaskResultManifest, &bytes)
        .map_err(BackupArtifactError::Contract)?;
    if manifest["task_id"] != result.task
        || manifest["attempt_id"] != attempt
        || manifest["input_fingerprint"] != result.input_fingerprint
    {
        return Err(invalid());
    }
    let parent = result.path.parent().ok_or_else(invalid)?;
    let entries = manifest["entries"].as_array().ok_or_else(invalid)?;
    for entry in entries {
        let relative = Path::new(entry["relative_path"].as_str().ok_or_else(invalid)?);
        if !is_portable_relative_file(relative) {
            return Err(BackupArtifactError::InvalidPath(relative.to_owned()));
        }
        let size = entry["size_bytes"].as_u64().ok_or_else(invalid)?;
        let digest = entry["sha256"].as_str().ok_or_else(invalid)?;
        stage.copy(&parent.join(relative), Some(digest), Some(size))?;
    }
    Ok(())
}

struct ArtifactStage<'a> {
    root: &'a Path,
    directory: Option<&'a Path>,
    files: BTreeMap<PathBuf, BackupArtifactFile>,
}

impl ArtifactStage<'_> {
    fn copy(
        &mut self,
        path: &Path,
        digest: Option<&str>,
        size: Option<u64>,
    ) -> Result<(), BackupArtifactError> {
        if !is_portable_relative_file(path)
            || !path.starts_with("artifacts")
            || path
                .parent()
                .is_none_or(|parent| parent.as_os_str().is_empty())
        {
            return Err(BackupArtifactError::InvalidPath(path.to_owned()));
        }
        if let Some(existing) = self.files.get(path) {
            return compare(path, existing, digest, size);
        }
        let workspace_path = path
            .iter()
            .map(|segment| segment.to_str().expect("validated UTF-8"))
            .collect::<Vec<_>>()
            .join("/");
        let mut source = open_scoped_file(self.root, path)?;
        if let Some(expected) = size
            && source.metadata()?.len() != expected
        {
            return Err(BackupArtifactError::DigestMismatch(path.to_owned()));
        }
        let (size_bytes, sha256) = if let Some(directory) = self.directory {
            let target = directory.join(path);
            fs::create_dir_all(target.parent().expect("validated artifact parent"))?;
            let mut output = File::create_new(target)?;
            let proof = copy_and_hash(&mut source, &mut output)?;
            output.sync_all()?;
            proof
        } else {
            copy_and_hash(&mut source, &mut io::sink())?
        };
        let record = BackupArtifactFile {
            package_path: workspace_path.clone(),
            workspace_path,
            sha256,
            size_bytes,
        };
        compare(path, &record, digest, size)?;
        self.files.insert(path.to_owned(), record);
        Ok(())
    }
}

fn compare(
    path: &Path,
    file: &BackupArtifactFile,
    digest: Option<&str>,
    size: Option<u64>,
) -> Result<(), BackupArtifactError> {
    if digest.is_some_and(|sha| sha != file.sha256)
        || size.is_some_and(|size| size != file.size_bytes)
    {
        return Err(BackupArtifactError::DigestMismatch(path.to_owned()));
    }
    Ok(())
}
