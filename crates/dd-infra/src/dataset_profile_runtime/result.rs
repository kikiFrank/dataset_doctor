//! Read the profile and optional file issues from the Workspace's accepted receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{CAPABILITY, DatasetProfileError, OPERATION};
use crate::artifact_manifest::{
    MANIFEST_FILE, TaskArtifactEntry, TaskManifestScope, parse_stored_manifest_reference,
    verify_task_result_manifest,
};
use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, JsonProfile, strict_json};
use crate::paths::open_scoped_file;
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, format_digest, parse_digest};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const PROFILE_FILE: &str = "dataset-profile.json";
const ERRORS_FILE: &str = "dataset-profile-file-errors.json";
const JSON_LIMIT: u64 = 16 * 1024 * 1024;
const KIND: &str = "CHECK_RESULT";
const MEDIA_TYPE: &str = "application/json";

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DatasetProfile {
    pub input_fingerprint: String,
    pub asset_count: usize,
    pub annotation_count: u64,
    pub resolution_histogram: Vec<ProfileResolution>,
    pub class_counts: BTreeMap<String, u64>,
    pub empty_annotation_count: usize,
    pub unannotated_count: usize,
    pub file_errors_ref: Option<ArtifactReference>,
    #[serde(skip)]
    pub file_errors: Vec<ProfileFileError>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileResolution {
    pub width: u32,
    pub height: u32,
    pub count: usize,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileFileError {
    pub asset_id: Uuid,
    pub source: String,
    pub code: String,
}

struct AcceptedProfile {
    root: PathBuf,
    attempt: Uuid,
    input: PathBuf,
    fingerprint: String,
    output: PathBuf,
    result_ref: String,
    started_at_ms: i64,
    finished_at_ms: i64,
}

pub(crate) struct VerifiedProfileEvidence {
    pub data: DatasetProfile,
    pub reference: ArtifactReference,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

pub fn read_dataset_profile(
    workspace: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    task: Uuid,
) -> Result<DatasetProfile, DatasetProfileError> {
    read_profile_evidence(workspace, session, dataset, task).map(|evidence| evidence.data)
}

pub(crate) fn read_profile_evidence(
    workspace: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    task: Uuid,
) -> Result<VerifiedProfileEvidence, DatasetProfileError> {
    let accepted = workspace.workspace_action(session, move |owner| {
        owner.connection.query_row(
            "SELECT current_attempt_id,input_manifest_path,input_fingerprint,output_manifest_path,
                    json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref'),
                    started_at_ms,finished_at_ms
             FROM tasks WHERE task_id=?1 AND dataset_id=?2 AND operation_kind=?3
                 AND capability_id=?4 AND status='SUCCEEDED'",
            params![task.to_string(), dataset.to_string(), OPERATION, CAPABILITY],
            |row| Ok(AcceptedProfile {
                root: owner.root().to_owned(), attempt: row_uuid(row, 0)?,
                input: PathBuf::from(row.get::<_, String>(1)?), fingerprint: row.get(2)?,
                output: PathBuf::from(row.get::<_, String>(3)?), result_ref: row.get(4)?,
                started_at_ms: row.get(5)?, finished_at_ms: row.get(6)?,
            }),
        ).optional().map_err(TaskStoreError::Database).map_err(WorkspaceThreadError::Task)?
         .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
    })?;
    let invalid = || DatasetProfileError::ResultUnconfirmed;
    let fingerprint = parse_digest(&accepted.fingerprint).ok_or_else(invalid)?;
    let contracts = JsonContracts::new().map_err(DatasetProfileError::TaskEvidence)?;
    let inputs = manifest_assets(&contracts, &accepted, dataset, fingerprint)?;
    let relative = PathBuf::from(format!("artifacts/tasks/{task}/{}", accepted.attempt));
    if accepted.output != relative.join(MANIFEST_FILE) {
        return Err(invalid());
    }
    let directory = accepted.root.join(relative);
    if directory.canonicalize()? != directory {
        return Err(invalid());
    }
    let receipt = parse_stored_manifest_reference(&accepted.result_ref, task).map_err(evidence)?;
    let scope = TaskManifestScope {
        task,
        attempt: accepted.attempt,
        input_fingerprint: fingerprint,
    };
    let verified =
        verify_task_result_manifest(&directory, &contracts, &scope, &receipt).map_err(evidence)?;
    let [summary, rest @ ..] = verified.entries() else {
        return Err(invalid());
    };
    let mut profile: DatasetProfile = read_entry(&directory, summary, PROFILE_FILE)?;
    match (profile.file_errors_ref.as_ref(), rest) {
        (None, []) => (),
        (Some(reference), [entry]) if reference_matches(task, reference, entry) => {
            profile.file_errors = read_entry(&directory, entry, ERRORS_FILE)?;
            if profile.file_errors.is_empty() {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    if profile.input_fingerprint != accepted.fingerprint || profile.asset_count != inputs.len() {
        return Err(invalid());
    }
    let mut unique_errors = BTreeSet::new();
    for error in &profile.file_errors {
        if !inputs.contains(&error.asset_id)
            || !matches!(error.source.as_str(), "image" | "annotation")
            || !unique_errors.insert((error.asset_id, error.source.as_str()))
        {
            return Err(invalid());
        }
    }
    let reference = ArtifactReference {
        artifact_id: receipt.artifact_id.parse().map_err(evidence)?,
        owner: ArtifactOwner::Task(task),
        kind: receipt.kind,
        media_type: receipt.media_type,
        sha256: receipt.sha256,
        size_bytes: receipt.size_bytes.to_string(),
        schema_id: receipt.schema_id,
    };
    Ok(VerifiedProfileEvidence {
        data: profile,
        reference,
        started_at_ms: accepted.started_at_ms,
        finished_at_ms: accepted.finished_at_ms,
    })
}

fn manifest_assets(
    contracts: &JsonContracts,
    accepted: &AcceptedProfile,
    dataset: Uuid,
    fingerprint: [u8; 32],
) -> Result<BTreeSet<Uuid>, DatasetProfileError> {
    let invalid = || DatasetProfileError::ResultUnconfirmed;
    let mut bytes = Vec::new();
    open_scoped_file(&accepted.root, &accepted.input)?
        .take(JSON_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > JSON_LIMIT {
        return Err(invalid());
    }
    let input = contracts
        .validate(JsonProfile::InputManifest, &bytes)
        .map_err(evidence)?;
    let frozen = freeze_input_manifest(contracts, input.clone())
        .map_err(DatasetProfileError::Fingerprint)?;
    if input["dataset_id"] != dataset.to_string()
        || input["capability_id"] != CAPABILITY
        || bytes != frozen.bytes
        || frozen.input_fingerprint != fingerprint
    {
        return Err(invalid());
    }
    input["assets"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|asset| {
            asset["asset_id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .ok_or_else(invalid)
        })
        .collect()
}

fn reference_matches(task: Uuid, reference: &ArtifactReference, entry: &TaskArtifactEntry) -> bool {
    reference.owner == ArtifactOwner::Task(task)
        && entry.artifact_id == reference.artifact_id.to_string()
        && entry.kind == reference.kind
        && entry.media_type == reference.media_type
        && entry.size_bytes.to_string() == reference.size_bytes
        && entry.sha256 == reference.sha256
        && entry.schema_id == reference.schema_id
}

fn read_entry<T: serde::de::DeserializeOwned>(
    directory: &Path,
    entry: &TaskArtifactEntry,
    expected_file: &str,
) -> Result<T, DatasetProfileError> {
    if entry.relative_path != expected_file
        || entry.kind != KIND
        || entry.media_type != MEDIA_TYPE
        || entry.size_bytes > JSON_LIMIT
    {
        return Err(DatasetProfileError::ResultUnconfirmed);
    }
    let mut bytes = Vec::new();
    open_scoped_file(directory, Path::new(expected_file))?
        .take(entry.size_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != entry.size_bytes
        || format_digest(&Sha256::digest(&bytes).into()) != entry.sha256
    {
        return Err(DatasetProfileError::ResultUnconfirmed);
    }
    let value = strict_json(&bytes).map_err(evidence)?;
    serde_json::from_value(value).map_err(evidence)
}

fn evidence(error: impl std::error::Error + Send + Sync + 'static) -> DatasetProfileError {
    DatasetProfileError::TaskEvidence(Box::new(error))
}
