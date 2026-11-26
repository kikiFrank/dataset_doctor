//! Resolve the decoded image facts associated with an accepted Workspace Task.

use std::error::Error;
use std::fmt::{self, Display};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use dd_protocol::v1::{ArtifactRef, artifact_ref::Owner};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::CAPABILITY;
use crate::artifact_manifest::{
    ArtifactValidationError, MANIFEST_FILE, TaskManifestScope, verify_task_result_manifest,
};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, JsonProfile, strict_json};
use crate::paths::open_scoped_file;
use crate::review_store::{ReviewStatus, ReviewStoreError};
use crate::task_store::{TaskStoreError, format_digest};
use crate::workspace::OpenedWorkspace;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

#[derive(Debug)]
pub enum ReviewImageEvidenceError {
    Storage(WorkspaceThreadError),
    Artifact(ArtifactValidationError),
    Io(io::Error),
    InvalidEvidence,
}

impl Display for ReviewImageEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => Display::fmt(error, formatter),
            Self::Artifact(error) => Display::fmt(error, formatter),
            Self::Io(error) => write!(formatter, "Read review image evidence: {error}"),
            Self::InvalidEvidence => {
                formatter.write_str("Accepted geometry does not match the active review baseline")
            }
        }
    }
}

impl Error for ReviewImageEvidenceError {}

impl From<io::Error> for ReviewImageEvidenceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct VerifiedReviewImage {
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

struct AcceptedGeometry {
    workspace_root: PathBuf,
    dataset_id: String,
    attempt: String,
    input_path: String,
    input_fingerprint: String,
    output_path: String,
    artifact_id: String,
    artifact_size: String,
    artifact_sha: String,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct GeometryReport {
    pub image: GeometryImage,
    pub status: String,
    pub findings: Vec<GeometryFinding>,
}

#[derive(Debug, Deserialize)]
pub struct GeometryImage {
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct GeometryFinding {
    pub code: String,
    pub annotation_id: Option<u64>,
}

/// Return only the report accepted by Core and reverify its bytes at read time.
pub fn read_geometry_report(
    workspace: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
) -> Result<GeometryReport, ReviewImageEvidenceError> {
    let accepted = workspace
        .workspace_action(session, move |owner| accepted_geometry(owner, task))
        .map_err(ReviewImageEvidenceError::Storage)?;
    let (_, report) = load_geometry(task, accepted)?;
    Ok(report)
}

fn accepted_geometry(
    owner: &OpenedWorkspace,
    task: Uuid,
) -> Result<AcceptedGeometry, WorkspaceThreadError> {
    owner.connection.query_row(
        "SELECT current_attempt_id, input_manifest_path, input_fingerprint, output_manifest_path,
                json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref.artifact_id'),
                json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref.size_bytes'),
                json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref.sha256'),
                dataset_id,started_at_ms,finished_at_ms
         FROM tasks WHERE task_id=?1 AND capability_id=?2 AND status='SUCCEEDED' AND dataset_id IS NOT NULL",
        params![task.to_string(), CAPABILITY],
        |row| Ok(AcceptedGeometry {
            workspace_root: owner.root().to_owned(), attempt: row.get(0)?,
            input_path: row.get(1)?, input_fingerprint: row.get(2)?, output_path: row.get(3)?,
            artifact_id: row.get(4)?, artifact_size: row.get(5)?, artifact_sha: row.get(6)?,
            dataset_id: row.get(7)?, started_at_ms: row.get(8)?, finished_at_ms: row.get(9)?,
        }),
    ).optional().map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Database(error)))?
    .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
}

pub(crate) struct BoundGeometryReport {
    pub report: GeometryReport,
    pub input_fingerprint: String,
    pub reference: crate::artifact_reference::ArtifactReference,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

pub(crate) fn read_bound_geometry_report(
    workspace: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
    image: &crate::dataset_query::RegisteredImage,
) -> Result<BoundGeometryReport, ReviewImageEvidenceError> {
    use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
    let accepted = workspace
        .workspace_action(session, move |owner| accepted_geometry(owner, task))
        .map_err(ReviewImageEvidenceError::Storage)?;
    let invalid = || ReviewImageEvidenceError::InvalidEvidence;
    let input_fingerprint = accepted.input_fingerprint.clone();
    let started_at_ms = accepted.started_at_ms.ok_or_else(invalid)?;
    let finished_at_ms = accepted.finished_at_ms.ok_or_else(invalid)?;
    let reference = ArtifactReference {
        artifact_id: Uuid::parse_str(&accepted.artifact_id).map_err(|_| invalid())?,
        owner: ArtifactOwner::Task(task),
        kind: "TASK_RESULT_MANIFEST".into(),
        media_type: "application/json".into(),
        sha256: accepted.artifact_sha.clone(),
        size_bytes: accepted.artifact_size.clone(),
        schema_id: None,
    };
    let (input, report) = load_geometry(task, accepted)?;
    if input["dataset_id"] != image.dataset_id.to_string()
        || input["assets"][0]["asset_id"] != image.asset_id.to_string()
        || input["label_schema_digest"] != image.label_schema_fingerprint
        || input["assets"][0]["annotation_digest"].as_str()
            != image.annotation.as_ref().map(|file| file.sha256.as_str())
        || report.image.sha256 != image.image.sha256
        || started_at_ms > finished_at_ms
    {
        return Err(invalid());
    }
    Ok(BoundGeometryReport {
        report,
        input_fingerprint,
        reference,
        started_at_ms,
        finished_at_ms,
    })
}

/// Use an accepted Task as evidence for the geometry editor. The result and
/// canonical input must still match the digests stored by the Core.
pub fn read_review_image(
    workspace: &WorkspaceThread,
    session: Uuid,
    review: Uuid,
    task: Uuid,
) -> Result<VerifiedReviewImage, ReviewImageEvidenceError> {
    let (accepted, review, image_name) = workspace
        .workspace_action(session, move |owner| {
            let review = owner
                .get_review(review)
                .map_err(WorkspaceThreadError::Review)?;
            if review.status != ReviewStatus::Open {
                return Err(WorkspaceThreadError::Review(ReviewStoreError::InvalidState));
            }
            let binding = owner
                .registered_image(review.dataset_id, review.asset_id)
                .map_err(WorkspaceThreadError::Dataset)?;
            if binding.image.sha256 != review.base_image_digest
                || binding.annotation.as_ref().map(|file| &file.sha256)
                    != review.base_annotation_digest.as_ref()
                || binding.label_schema_fingerprint != review.base_label_schema_digest
            {
                return Err(WorkspaceThreadError::Review(
                    ReviewStoreError::RevisionConflict,
                ));
            }
            let image_name = binding
                .image
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(WorkspaceThreadError::Review(
                    ReviewStoreError::InvalidRecord,
                ))?
                .to_owned();
            Ok((accepted_geometry(owner, task)?, review, image_name))
        })
        .map_err(ReviewImageEvidenceError::Storage)?;
    let (input, report) = load_geometry(task, accepted)?;
    if input["dataset_id"] != review.dataset_id.to_string()
        || input["label_schema_digest"] != review.base_label_schema_digest
        || input["assets"][0]["asset_id"] != review.asset_id.to_string()
        || report.image.sha256 != review.base_image_digest
        || input["assets"][0]["annotation_digest"].as_str()
            != review.base_annotation_digest.as_deref()
    {
        return Err(ReviewImageEvidenceError::InvalidEvidence);
    }
    Ok(VerifiedReviewImage {
        file_name: image_name,
        width: report.image.width,
        height: report.image.height,
        sha256: report.image.sha256,
    })
}

fn load_geometry(
    task: Uuid,
    accepted: AcceptedGeometry,
) -> Result<(serde_json::Value, GeometryReport), ReviewImageEvidenceError> {
    let invalid = || ReviewImageEvidenceError::InvalidEvidence;
    let contracts = JsonContracts::new().map_err(|_| invalid())?;
    let input_bytes = read_scoped(
        &accepted.workspace_root,
        Path::new(&accepted.input_path),
        64 * 1024,
    )?;
    let input = contracts
        .validate(JsonProfile::InputManifest, &input_bytes)
        .map_err(|_| invalid())?;
    let frozen = freeze_input_manifest(&contracts, input.clone()).map_err(|_| invalid())?;
    let assets = input["assets"].as_array().ok_or_else(invalid)?;
    if input_bytes != frozen.bytes
        || format_digest(&frozen.input_fingerprint) != accepted.input_fingerprint
        || input["dataset_id"] != accepted.dataset_id
        || input["capability_id"] != CAPABILITY
        || assets.len() != 1
    {
        return Err(invalid());
    }
    let attempt = Uuid::parse_str(&accepted.attempt).map_err(|_| invalid())?;
    let directory = PathBuf::from(format!("artifacts/tasks/{task}/{attempt}"));
    if Path::new(&accepted.output_path) != directory.join(MANIFEST_FILE) {
        return Err(invalid());
    }
    let approved = accepted.workspace_root.join(directory);
    if approved.canonicalize()? != approved {
        return Err(invalid());
    }
    let verified = verify_task_result_manifest(
        &approved,
        &contracts,
        &TaskManifestScope {
            task,
            attempt,
            input_fingerprint: frozen.input_fingerprint,
        },
        &ArtifactRef {
            artifact_id: accepted.artifact_id,
            owner: Some(Owner::OwnerTaskId(task.to_string())),
            kind: "TASK_RESULT_MANIFEST".into(),
            media_type: "application/json".into(),
            size_bytes: accepted.artifact_size.parse().map_err(|_| invalid())?,
            sha256: accepted.artifact_sha,
            schema_id: None,
        },
    )
    .map_err(ReviewImageEvidenceError::Artifact)?;
    let [entry] = verified.entries() else {
        return Err(invalid());
    };
    if entry.relative_path != "geometry-result.json"
        || entry.kind != "CHECK_RESULT"
        || entry.media_type != "application/json"
        || entry.size_bytes > 64 * 1024 * 1024
    {
        return Err(invalid());
    }
    let bytes = read_scoped(&approved, Path::new(&entry.relative_path), entry.size_bytes)?;
    if bytes.len() as u64 != entry.size_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
    {
        return Err(invalid());
    }
    let value = strict_json(&bytes).map_err(|_| invalid())?;
    let result: GeometryReport = serde_json::from_value(value).map_err(|_| invalid())?;
    if assets[0]["media_digest"].as_str() != Some(result.image.sha256.as_str())
        || result.image.width == 0
        || result.image.height == 0
    {
        return Err(invalid());
    }
    Ok((input, result))
}

fn read_scoped(root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>, ReviewImageEvidenceError> {
    let mut bytes = Vec::new();
    open_scoped_file(root, path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(ReviewImageEvidenceError::InvalidEvidence);
    }
    Ok(bytes)
}
