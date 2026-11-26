//! Verify sealed task output before Core accepts or indexes Worker artifacts.

use std::collections::HashSet;
use std::error::Error;
use std::fmt::{self, Display};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use dd_protocol::v1::{ArtifactRef, artifact_ref::Owner};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::json::{ContractError, JsonContracts, JsonProfile};
use crate::paths::{is_portable_relative_file, open_scoped_file};
use crate::task_store::format_digest;

pub(crate) const MANIFEST_FILE: &str = "result-manifest.json";
const MANIFEST_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskManifestScope {
    pub task: Uuid,
    pub attempt: Uuid,
    pub input_fingerprint: [u8; 32],
}

#[derive(Debug)]
pub enum ArtifactValidationError {
    InvalidReference,
    ScopeMismatch,
    InvalidEntryPath,
    DuplicateEntry,
    DigestMismatch,
    Io(io::Error),
    Contract(ContractError),
    Mapping(serde_json::Error),
}

impl Display for ArtifactValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference => formatter.write_str("Invalid task result manifest reference"),
            Self::ScopeMismatch => {
                formatter.write_str("Task result belongs to another input or attempt")
            }
            Self::InvalidEntryPath => {
                formatter.write_str("Task result entry is outside its artifact scope")
            }
            Self::DuplicateEntry => {
                formatter.write_str("Task result contains a duplicate artifact ID or path")
            }
            Self::DigestMismatch => formatter.write_str("Task result file size or digest changed"),
            Self::Io(error) => write!(formatter, "Read task artifacts: {error}"),
            Self::Contract(error) => Display::fmt(error, formatter),
            Self::Mapping(error) => write!(formatter, "Read task manifest fields: {error}"),
        }
    }
}

impl Error for ArtifactValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Contract(error) => Some(error),
            Self::Mapping(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ArtifactValidationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TaskArtifactEntry {
    pub artifact_id: String,
    pub relative_path: String,
    pub kind: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub schema_id: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    task_id: String,
    attempt_id: String,
    input_fingerprint: String,
    entries: Vec<TaskArtifactEntry>,
}

#[derive(Clone, Debug)]
pub struct VerifiedTaskManifest {
    pub(crate) scope: TaskManifestScope,
    pub(crate) path: PathBuf,
    reference: ArtifactRef,
    entries: Vec<TaskArtifactEntry>,
}

impl VerifiedTaskManifest {
    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub fn entries(&self) -> &[TaskArtifactEntry] {
        &self.entries
    }
}

/// The attempt directory comes from Core's remembered grant scope. The Worker
/// supplies only a reference and portable names inside the sealed manifest.
pub fn verify_task_result_manifest(
    approved_attempt_directory: &Path,
    contracts: &JsonContracts,
    scope: &TaskManifestScope,
    reference: &ArtifactRef,
) -> Result<VerifiedTaskManifest, ArtifactValidationError> {
    let task = scope.task.to_string();
    let manifest_id = Uuid::parse_str(&reference.artifact_id)
        .map_err(|_| ArtifactValidationError::InvalidReference)?;
    if reference.owner.as_ref() != Some(&Owner::OwnerTaskId(task.clone()))
        || reference.kind != "TASK_RESULT_MANIFEST"
        || reference.media_type != "application/json"
        || reference.size_bytes > MANIFEST_LIMIT
        || manifest_id.to_string() != reference.artifact_id
    {
        return Err(ArtifactValidationError::InvalidReference);
    }

    let root = approved_attempt_directory.canonicalize()?;
    let manifest_path = Path::new(MANIFEST_FILE);
    let mut bytes = Vec::new();
    open_scoped_file(&root, manifest_path)?
        .take(reference.size_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != reference.size_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != reference.sha256
    {
        return Err(ArtifactValidationError::DigestMismatch);
    }
    let validated = contracts
        .validate(JsonProfile::TaskResultManifest, &bytes)
        .map_err(ArtifactValidationError::Contract)?;
    let manifest: Manifest =
        serde_json::from_value(validated).map_err(ArtifactValidationError::Mapping)?;
    if manifest.task_id != task
        || manifest.attempt_id != scope.attempt.to_string()
        || manifest.input_fingerprint != format_digest(&scope.input_fingerprint)
    {
        return Err(ArtifactValidationError::ScopeMismatch);
    }

    let mut identifiers = HashSet::from([manifest_id]);
    let mut paths = HashSet::from([root.join(manifest_path).canonicalize()?]);
    for entry in &manifest.entries {
        let path = Path::new(&entry.relative_path);
        if !is_portable_relative_file(path) {
            return Err(ArtifactValidationError::InvalidEntryPath);
        }
        let id = Uuid::parse_str(&entry.artifact_id)
            .map_err(|_| ArtifactValidationError::InvalidReference)?;
        let file = open_scoped_file(&root, path).map_err(|error| match error.kind() {
            io::ErrorKind::PermissionDenied => ArtifactValidationError::InvalidEntryPath,
            _ => ArtifactValidationError::Io(error),
        })?;
        if !identifiers.insert(id) || !paths.insert(root.join(path).canonicalize()?) {
            return Err(ArtifactValidationError::DuplicateEntry);
        }
        if file.metadata()?.len() != entry.size_bytes {
            return Err(ArtifactValidationError::DigestMismatch);
        }
        let mut digest = Sha256::new();
        let size = io::copy(
            &mut file.take(entry.size_bytes.saturating_add(1)),
            &mut digest,
        )?;
        if size != entry.size_bytes || format!("{:x}", digest.finalize()) != entry.sha256 {
            return Err(ArtifactValidationError::DigestMismatch);
        }
    }
    Ok(VerifiedTaskManifest {
        scope: *scope,
        path: root.join(manifest_path),
        reference: reference.clone(),
        entries: manifest.entries,
    })
}
