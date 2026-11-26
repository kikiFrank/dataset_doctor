//! Explicit approval and durable intent for publishing a reviewed COCO file.

mod apply;
mod tasks;
pub use tasks::FinalTaskSnapshot;

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_reference::ArtifactOwner;
use crate::dataset_query::RegisteredImage;
use crate::input_fingerprint::{InputFingerprintError, freeze_input_manifest};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::review_draft::{ReviewDraftError, ReviewDraftService, ReviewTrackBinding, draft_path};
use crate::review_store::{ReviewSession, ReviewStatus, ReviewStoreError};
use crate::review_validation::{ReviewValidationError, confirm_negative};
use crate::task_store::{TaskSubmission, enqueue_task_in, format_digest};
use crate::track_map::{TrackMap, TrackMapEntry};
use crate::video_keyframes::VideoAnnotationService;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const KIND: &str = "REVIEW_COMMIT";
const CAPABILITY: &str = "review_commit.v1";
const MAX_ANNOTATION: u64 = 16 * 1024 * 1024;
const MAX_IMAGE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ReviewFinalRequest {
    pub review_id: Uuid,
    pub expected_review_revision: u64,
    pub base_image_digest: String,
    pub base_annotation_digest: Option<String>,
    pub draft_digest: String,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub confirmed_negative: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalOperationState {
    Prepared,
    Committed,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalOperation {
    pub operation_id: Uuid,
    pub state: FinalOperationState,
}

#[derive(Debug)]
pub enum ReviewFinalError {
    Storage(WorkspaceThreadError),
    Draft(ReviewDraftError),
    Approval(ReviewValidationError),
    Input(InputFingerprintError),
    Json(serde_json::Error),
    Io(io::Error),
    Conflict,
    InvalidOperation,
}

impl Display for ReviewFinalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => Display::fmt(error, formatter),
            Self::Draft(error) => Display::fmt(error, formatter),
            Self::Approval(error) => Display::fmt(error, formatter),
            Self::Input(error) => Display::fmt(error, formatter),
            Self::Json(error) => write!(formatter, "Encode Final operation: {error}"),
            Self::Io(error) => write!(formatter, "Prepare Final files: {error}"),
            Self::Conflict => formatter.write_str(
                "Final request conflicts with the draft, source files or a previous operation",
            ),
            Self::InvalidOperation => formatter.write_str("Invalid Final operation record"),
        }
    }
}

impl Error for ReviewFinalError {}

impl From<io::Error> for ReviewFinalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct ReviewFinalService {
    owner: Arc<WorkspaceThread>,
    drafts: ReviewDraftService,
}

#[derive(Serialize, Deserialize)]
struct FinalManifest {
    contract_version: u8,
    operation_id: String,
    review_id: String,
    dataset_id: String,
    asset_id: String,
    asset_revision: i64,
    review_revision: i64,
    label_schema_digest: String,
    image_path: PathBuf,
    old_image_digest: String,
    target_path: PathBuf,
    registry_path: PathBuf,
    old_annotation_digest: Option<String>,
    new_annotation_digest: String,
    payload_path: String,
    local_image_id: i64,
    confirmed_negative: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    track_map: Option<TrackMapEntry>,
}

struct FinalTarget {
    root: PathBuf,
    review: ReviewSession,
    binding: RegisteredImage,
    location: String,
    asset_revision: i64,
}

impl ReviewFinalService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, ReviewFinalError> {
        let drafts =
            ReviewDraftService::new(Arc::clone(&owner)).map_err(ReviewFinalError::Draft)?;
        Ok(Self { owner, drafts })
    }

    /// Publish verified staging data and atomically record the user's intent.
    /// The official file remains untouched until the operation is applied.
    pub fn prepare(
        &self,
        session: Uuid,
        request: ReviewFinalRequest,
        now_ms: i64,
    ) -> Result<FinalOperation, ReviewFinalError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(ReviewFinalError::Storage)?;
        let key = request.idempotency_key.to_string();
        let request_digest = request_fingerprint(&request)?;
        let fingerprint = format_digest(&request_digest);
        let old_key = key.clone();
        let previous = self
            .owner
            .workspace_action(session, move |owner| {
                existing(&owner.connection, &old_key).map_err(database)
            })
            .map_err(ReviewFinalError::Storage)?;
        if let Some(previous) = previous {
            return matching_operation(previous, &fingerprint);
        }

        let review_id = request.review_id;
        let target = self
            .owner
            .workspace_action(session, move |owner| {
                let review = owner
                    .get_review(review_id)
                    .map_err(WorkspaceThreadError::Review)?;
                let binding = owner
                    .registered_image(review.dataset_id, review.asset_id)
                    .map_err(WorkspaceThreadError::Dataset)?;
                let (location, asset_revision) = owner
                    .connection
                    .query_row(
                        "SELECT location_kind, row_revision FROM assets WHERE asset_id=?1",
                        [review.asset_id.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(database)?;
                Ok(FinalTarget {
                    root: owner.root().to_owned(),
                    review,
                    binding,
                    location,
                    asset_revision,
                })
            })
            .map_err(ReviewFinalError::Storage)?;
        let review = &target.review;
        if review.status != ReviewStatus::Open
            || review.row_revision != request.expected_review_revision
            || review.base_image_digest != request.base_image_digest
            || review.base_annotation_digest != request.base_annotation_digest
            || target.binding.image.sha256 != review.base_image_digest
            || target.binding.annotation.as_ref().map(|file| &file.sha256)
                != review.base_annotation_digest.as_ref()
            || target.binding.label_schema_fingerprint != review.base_label_schema_digest
        {
            return Err(ReviewFinalError::Conflict);
        }
        let draft = self
            .drafts
            .read(session, request.review_id)
            .map_err(ReviewFinalError::Draft)?
            .filter(|draft| {
                draft.receipt.review_revision == request.expected_review_revision
                    && draft.receipt.draft_digest == request.draft_digest
            })
            .ok_or(ReviewFinalError::Conflict)?;
        let annotations = draft.coco_document["annotations"]
            .as_array()
            .ok_or(ReviewFinalError::InvalidOperation)?;
        confirm_negative(!annotations.is_empty(), request.confirmed_negative)
            .map_err(ReviewFinalError::Approval)?;
        let local_image_id = draft.coco_document["images"][0]["id"]
            .as_i64()
            .ok_or(ReviewFinalError::InvalidOperation)?;

        let image = &target.binding.image.path;
        let annotation = match &target.binding.annotation {
            Some(existing) => existing.path.clone(),
            None => new_annotation_path(image)?,
        };
        if file_digest(image, MAX_IMAGE)?.as_deref() != Some(review.base_image_digest.as_str())
            || file_digest(&annotation, MAX_ANNOTATION)? != review.base_annotation_digest
        {
            return Err(ReviewFinalError::Conflict);
        }
        let registry_path = match target.location.as_str() {
            "EXTERNAL" => annotation.clone(),
            "WORKSPACE" => annotation
                .strip_prefix(&target.root)
                .map_err(|_| ReviewFinalError::Conflict)?
                .into(),
            _ => return Err(ReviewFinalError::InvalidOperation),
        };
        let operation_id = Uuid::new_v4();
        let prefix = format!("artifacts/file-operations/{operation_id}");
        let payload_path = format!("{prefix}/annotation.json");
        let payload = serde_json::to_vec(&draft.coco_document).map_err(ReviewFinalError::Json)?;
        let new_annotation_digest = digest(&payload);
        let track_map = self.prepare_track_map(
            session,
            &target,
            operation_id,
            &new_annotation_digest,
            draft.track_bindings,
        )?;
        let review_revision =
            i64::try_from(review.row_revision).map_err(|_| ReviewFinalError::InvalidOperation)?;
        let next_review_revision = review_revision
            .checked_add(1)
            .ok_or(ReviewFinalError::InvalidOperation)?;
        let manifest = FinalManifest {
            contract_version: 1,
            operation_id: operation_id.to_string(),
            review_id: review.review_id.to_string(),
            dataset_id: review.dataset_id.to_string(),
            asset_id: review.asset_id.to_string(),
            asset_revision: target.asset_revision,
            review_revision: next_review_revision,
            label_schema_digest: review.base_label_schema_digest.clone(),
            image_path: image.clone(),
            old_image_digest: review.base_image_digest.clone(),
            target_path: annotation,
            registry_path,
            old_annotation_digest: review.base_annotation_digest.clone(),
            new_annotation_digest,
            payload_path,
            local_image_id,
            confirmed_negative: request.confirmed_negative,
            track_map,
        };
        let manifest_path = format!("{prefix}/operation.json");
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(ReviewFinalError::Json)?;
        let manifest_digest = digest(&manifest_bytes);
        publish_immutable(&target.root, Path::new(&manifest.payload_path), &payload)?;
        publish_immutable(&target.root, Path::new(&manifest_path), &manifest_bytes)?;

        let input = freeze_input_manifest(&self.drafts.contracts, serde_json::json!({
            "capability_id": CAPABILITY, "implementation_version": "1",
            "dataset_id": manifest.dataset_id, "label_schema_digest": manifest.label_schema_digest,
            "assets": [{"asset_id": manifest.asset_id, "media_digest": manifest.old_image_digest,
                "annotation_digest": manifest.old_annotation_digest, "source_metadata_digest": null}],
            "model_identity": null, "resolved_config": {
                "review_id": manifest.review_id, "draft_digest": request.draft_digest,
                "expected_review_revision": request.expected_review_revision.to_string(),
                "operation_manifest_digest": manifest_digest,
                "confirmed_negative": request.confirmed_negative,
            },
        })).map_err(ReviewFinalError::Input)?;
        let input_path = PathBuf::from(format!("{prefix}/input.json"));
        publish_immutable(&target.root, &input_path, &input.bytes)?;
        let submission = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(review.dataset_id),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: KIND.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: input_path,
            request_fingerprint: request_digest,
            input_fingerprint: input.input_fingerprint,
        };

        let draft_path = draft_path(review_id, &request.draft_digest);
        let dataset_id = review.dataset_id;
        let asset_id = review.asset_id;
        self.owner.workspace_action(session, move |owner| {
            let binding = owner.registered_image(dataset_id, asset_id).map_err(WorkspaceThreadError::Dataset)?;
            if binding.label_schema_fingerprint != manifest.label_schema_digest
                || binding.image.sha256 != manifest.old_image_digest
                || binding.annotation.as_ref().map(|file| &file.sha256) != manifest.old_annotation_digest.as_ref()
            {
                return Ok(Err(ReviewFinalError::Conflict));
            }
            let transaction = owner.connection.transaction().map_err(database)?;
            if let Some(previous) = existing(&transaction, &key).map_err(database)? {
                return Ok(matching_operation(previous, &fingerprint));
            }
            let changed = transaction.execute(
                "UPDATE review_sessions SET status='COMMITTING', row_revision=?1, updated_at_ms=?2
                 WHERE review_id=?3 AND status='OPEN' AND row_revision=?4 AND review_manifest_path=?5
                 AND EXISTS (SELECT 1 FROM assets WHERE asset_id=?6 AND row_revision=?7)",
                params![next_review_revision, now_ms, manifest.review_id, review_revision, draft_path,
                    manifest.asset_id, manifest.asset_revision],
            ).map_err(database)?;
            if changed != 1 {
                return Ok(Err(ReviewFinalError::Conflict));
            }
            let task = enqueue_task_in(&transaction, &submission, now_ms).map_err(WorkspaceThreadError::Task)?;
            transaction.execute(
                "INSERT INTO file_operations (operation_id, idempotency_key, request_id, request_fingerprint,
                    workspace_session_id, operation_kind, dataset_id, asset_id, review_id, task_id, status,
                    manifest_path, manifest_digest, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'PREPARED', ?11, ?12, ?13, ?13)",
                params![manifest.operation_id, key, request.request_id.to_string(), fingerprint, session.to_string(), KIND,
                    manifest.dataset_id, manifest.asset_id, manifest.review_id, task.task_id.to_string(), manifest_path, manifest_digest, now_ms],
            ).map_err(database)?;
            transaction.commit().map_err(database)?;
            Ok(Ok(FinalOperation { operation_id, state: FinalOperationState::Prepared }))
        }).map_err(ReviewFinalError::Storage)?
    }

    fn prepare_track_map(
        &self,
        session: Uuid,
        target: &FinalTarget,
        operation: Uuid,
        annotation_digest: &str,
        bindings: Vec<ReviewTrackBinding>,
    ) -> Result<Option<TrackMapEntry>, ReviewFinalError> {
        if bindings.is_empty() {
            return Ok(None);
        }
        let invalid = || ReviewFinalError::Draft(ReviewDraftError::InvalidTrackingSelection);
        let work = VideoAnnotationService::new(Arc::clone(&self.owner)).map_err(|_| invalid())?;
        let candidates = work
            .staging_tracking_candidates_locked(
                session,
                target.review.dataset_id,
                target.review.asset_id,
            )
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        let ArtifactOwner::Task(task) = candidates.reference.owner else {
            return Err(invalid());
        };
        let observed: BTreeSet<_> = candidates
            .observations
            .iter()
            .map(|item| item.track_id)
            .collect();
        if bindings.iter().any(|binding| {
            binding.tracking_task_id != task
                || binding.tracking_attempt_id != candidates.tracking_attempt_id
                || !observed.contains(&binding.track_id)
        }) {
            return Err(invalid());
        }
        let map = TrackMap {
            contract_version: 1,
            asset_id: target.review.asset_id,
            annotation_digest: annotation_digest.to_owned(),
            segment_id: candidates.segment_id,
            frame: candidates.frame,
            bindings,
        };
        Ok(Some(TrackMapEntry::publish(
            &target.root,
            &self.drafts.contracts,
            &map,
            operation,
        )?))
    }
}

fn existing(
    connection: &Connection,
    key: &str,
) -> rusqlite::Result<Option<(String, String, String)>> {
    connection.query_row(
        "SELECT operation_id, request_fingerprint, status FROM file_operations WHERE operation_kind=?1 AND idempotency_key=?2",
        params![KIND, key], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()
}

fn matching_operation(
    (operation_id, fingerprint, status): (String, String, String),
    expected_fingerprint: &str,
) -> Result<FinalOperation, ReviewFinalError> {
    if fingerprint != expected_fingerprint {
        return Err(ReviewFinalError::Conflict);
    }
    operation_state(&operation_id, &status)
}

fn operation_state(operation_id: &str, status: &str) -> Result<FinalOperation, ReviewFinalError> {
    let state = match status {
        "PREPARED" => FinalOperationState::Prepared,
        "COMMITTED" => FinalOperationState::Committed,
        "CONFLICT" => FinalOperationState::Conflict,
        _ => return Err(ReviewFinalError::InvalidOperation),
    };
    let operation_id =
        Uuid::parse_str(operation_id).map_err(|_| ReviewFinalError::InvalidOperation)?;
    Ok(FinalOperation {
        operation_id,
        state,
    })
}

fn request_fingerprint(request: &ReviewFinalRequest) -> Result<[u8; 32], ReviewFinalError> {
    let fields = (
        "datasetdoctor.review-commit.v1",
        request.review_id.to_string(),
        request.expected_review_revision,
        &request.base_image_digest,
        &request.base_annotation_digest,
        &request.draft_digest,
        request.confirmed_negative,
    );
    let bytes = serde_json::to_vec(&fields).map_err(ReviewFinalError::Json)?;
    Ok(Sha256::digest(bytes).into())
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Review(ReviewStoreError::Database(error))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_digest(path: &Path, limit: u64) -> Result<Option<String>, ReviewFinalError> {
    use std::io::Read;
    let parent = verified_parent(path)?;
    let name = path.file_name().ok_or(ReviewFinalError::InvalidOperation)?;
    let file = match open_scoped_file(parent, Path::new(name)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut sha = Sha256::new();
    if io::copy(&mut file.take(limit + 1), &mut sha)? > limit {
        return Err(ReviewFinalError::Conflict);
    }
    Ok(Some(format!("{:x}", sha.finalize())))
}

fn verified_parent(path: &Path) -> Result<&Path, ReviewFinalError> {
    let parent = path
        .parent()
        .filter(|parent| parent.is_absolute())
        .ok_or(ReviewFinalError::InvalidOperation)?;
    if parent.canonicalize()? != parent {
        return Err(ReviewFinalError::Conflict);
    }
    Ok(parent)
}

fn new_annotation_path(image: &Path) -> Result<PathBuf, ReviewFinalError> {
    let parent = verified_parent(image)?;
    let stem = image
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or(ReviewFinalError::InvalidOperation)?;
    for entry in fs::read_dir(parent)? {
        let other = entry?.path();
        if other != image
            && other
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    if cfg!(windows) {
                        name.eq_ignore_ascii_case(stem)
                    } else {
                        name == stem
                    }
                })
            && other
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    ["jpg", "jpeg", "png"]
                        .iter()
                        .any(|supported| extension.eq_ignore_ascii_case(supported))
                })
        {
            let mut name = image
                .file_name()
                .ok_or(ReviewFinalError::InvalidOperation)?
                .to_owned();
            name.push(".json");
            return Ok(parent.join(name));
        }
    }
    Ok(image.with_extension("json"))
}
