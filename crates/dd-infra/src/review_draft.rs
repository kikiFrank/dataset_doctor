//! Store one recoverable, validated human draft at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Display};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_reference::ArtifactOwner;
use crate::geometry_runtime::{ReviewImageEvidenceError, read_review_image};
use crate::import_preflight::LocalCategory;
use crate::json::{ContractError, JsonContracts, JsonProfile};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::review_store::{ReviewSession, ReviewStatus, ReviewStoreError};
use crate::review_validation::{ReviewImage, ReviewValidationError, validate_review_document};
use crate::video_keyframes::VideoAnnotationService;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const DRAFT_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ReviewDraftRequest {
    pub review_id: Uuid,
    pub source_task_id: Uuid,
    pub expected_review_revision: u64,
    pub expected_draft_revision: Option<u64>,
    pub coco_document: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct ReviewTrackSelection {
    pub local_annotation_id: u64,
    pub track_id: Uuid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTrackBinding {
    pub local_annotation_id: u64,
    pub track_id: Uuid,
    pub tracking_task_id: Uuid,
    pub tracking_attempt_id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftReceipt {
    pub review_id: Uuid,
    pub draft_revision: u64,
    pub draft_digest: String,
    pub review_revision: u64,
}

#[derive(Debug)]
pub struct StoredDraft {
    pub receipt: DraftReceipt,
    pub source_task_id: Uuid,
    pub coco_document: Value,
    pub track_bindings: Vec<ReviewTrackBinding>,
}

pub struct ReviewEditorState {
    pub review: ReviewSession,
    pub categories: Vec<LocalCategory>,
    pub draft: Option<StoredDraft>,
}

#[derive(Debug)]
pub enum ReviewDraftError {
    Setup(Box<dyn Error + Send + Sync>),
    Storage(WorkspaceThreadError),
    Evidence(ReviewImageEvidenceError),
    Validation(ReviewValidationError),
    Contract(ContractError),
    Json(serde_json::Error),
    Io(io::Error),
    RevisionConflict,
    InvalidArtifact,
    InvalidTrackingSelection,
}

impl Display for ReviewDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "Initialize draft schemas: {error}"),
            Self::Storage(error) => Display::fmt(error, formatter),
            Self::Evidence(error) => Display::fmt(error, formatter),
            Self::Validation(error) => Display::fmt(error, formatter),
            Self::Contract(error) => Display::fmt(error, formatter),
            Self::Json(error) => write!(formatter, "Read or encode review draft: {error}"),
            Self::Io(error) => write!(formatter, "Access review draft file: {error}"),
            Self::RevisionConflict => {
                formatter.write_str("Draft or review changed since it was read")
            }
            Self::InvalidArtifact => {
                formatter.write_str("Draft file does not match the recorded review")
            }
            Self::InvalidTrackingSelection => {
                formatter.write_str("Selected track is unavailable for this annotation and frame")
            }
        }
    }
}

impl Error for ReviewDraftError {}

impl From<io::Error> for ReviewDraftError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct ReviewDraftService {
    owner: Arc<WorkspaceThread>,
    pub(crate) contracts: JsonContracts,
}

struct DraftSnapshot {
    root: PathBuf,
    session: ReviewSession,
    path: Option<String>,
    categories: String,
}

#[derive(Serialize, Deserialize)]
struct DraftArtifact {
    contract_version: u8,
    review_id: String,
    asset_id: String,
    source_task_id: String,
    draft_revision: String,
    image_digest: String,
    base_annotation_digest: Option<String>,
    base_label_schema_digest: String,
    coco_document: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    track_bindings: Vec<ReviewTrackBinding>,
}

impl ReviewDraftService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, ReviewDraftError> {
        let contracts = JsonContracts::new().map_err(ReviewDraftError::Setup)?;
        Ok(Self { owner, contracts })
    }

    pub fn read(
        &self,
        session: Uuid,
        review: Uuid,
    ) -> Result<Option<StoredDraft>, ReviewDraftError> {
        let snapshot = self.snapshot(session, review)?;
        self.load(&snapshot)
    }

    pub fn editor_state(
        &self,
        session: Uuid,
        review: Uuid,
    ) -> Result<ReviewEditorState, ReviewDraftError> {
        let snapshot = self.snapshot(session, review)?;
        if format!("{:x}", Sha256::digest(snapshot.categories.as_bytes()))
            != snapshot.session.base_label_schema_digest
        {
            return Err(ReviewDraftError::RevisionConflict);
        }
        let draft = self.load(&snapshot)?;
        let categories =
            serde_json::from_str(&snapshot.categories).map_err(ReviewDraftError::Json)?;
        Ok(ReviewEditorState {
            review: snapshot.session,
            categories,
            draft,
        })
    }

    pub fn save(
        &self,
        session: Uuid,
        request: ReviewDraftRequest,
        now_ms: i64,
    ) -> Result<DraftReceipt, ReviewDraftError> {
        self.save_scoped(session, request, None, now_ms)
    }

    pub fn save_with_tracks(
        &self,
        session: Uuid,
        request: ReviewDraftRequest,
        tracks: &[ReviewTrackSelection],
        now_ms: i64,
    ) -> Result<DraftReceipt, ReviewDraftError> {
        self.save_scoped(session, request, Some(tracks), now_ms)
    }

    fn save_scoped(
        &self,
        session: Uuid,
        request: ReviewDraftRequest,
        tracks: Option<&[ReviewTrackSelection]>,
        now_ms: i64,
    ) -> Result<DraftReceipt, ReviewDraftError> {
        let snapshot = self.snapshot(session, request.review_id)?;
        let current = &snapshot.session;
        if current.status != ReviewStatus::Open
            || current.row_revision != request.expected_review_revision
        {
            return Err(ReviewDraftError::RevisionConflict);
        }
        let draft = self.load(&snapshot)?;
        let old_revision = draft.as_ref().map(|draft| draft.receipt.draft_revision);
        if old_revision != request.expected_draft_revision {
            return Err(ReviewDraftError::RevisionConflict);
        }
        if request.coco_document.len() as u64 > DRAFT_LIMIT
            || format!("{:x}", Sha256::digest(snapshot.categories.as_bytes()))
                != current.base_label_schema_digest
        {
            return Err(ReviewDraftError::InvalidArtifact);
        }
        let image = read_review_image(
            &self.owner,
            session,
            request.review_id,
            request.source_task_id,
        )
        .map_err(ReviewDraftError::Evidence)?;
        let categories: Vec<LocalCategory> =
            serde_json::from_str(&snapshot.categories).map_err(ReviewDraftError::Json)?;
        let document = validate_review_document(
            &self.contracts,
            &request.coco_document,
            ReviewImage {
                file_name: &image.file_name,
                width: image.width,
                height: image.height,
            },
            &categories,
        )
        .map_err(ReviewDraftError::Validation)?;
        let coco_document = document.into_coco();
        let track_bindings = match tracks {
            Some(tracks) => self.bind_tracks(session, current, &coco_document, tracks)?,
            None => draft
                .as_ref()
                .map_or_else(Vec::new, |old| retain_tracks(old, &coco_document)),
        };
        let draft_revision = old_revision
            .unwrap_or_default()
            .checked_add(1)
            .ok_or(ReviewDraftError::InvalidArtifact)?;
        let review_revision = current
            .row_revision
            .checked_add(1)
            .filter(|revision| i64::try_from(*revision).is_ok())
            .ok_or(ReviewDraftError::InvalidArtifact)?;
        let artifact = DraftArtifact {
            contract_version: 1,
            review_id: current.review_id.to_string(),
            asset_id: current.asset_id.to_string(),
            source_task_id: request.source_task_id.to_string(),
            draft_revision: draft_revision.to_string(),
            image_digest: current.base_image_digest.clone(),
            base_annotation_digest: current.base_annotation_digest.clone(),
            base_label_schema_digest: current.base_label_schema_digest.clone(),
            coco_document,
            track_bindings,
        };
        let bytes = serde_json::to_vec(&artifact).map_err(ReviewDraftError::Json)?;
        if bytes.len() as u64 > DRAFT_LIMIT {
            return Err(ReviewDraftError::InvalidArtifact);
        }
        self.contracts
            .validate(JsonProfile::ReviewDraft, &bytes)
            .map_err(ReviewDraftError::Contract)?;
        let draft_digest = format!("{:x}", Sha256::digest(&bytes));
        let path = draft_path(current.review_id, &draft_digest);
        publish_immutable(&snapshot.root, Path::new(&path), &bytes)?;

        let old_path = snapshot.path;
        let expected = current.clone();
        let changed = self.owner.workspace_action(session, move |owner| {
            let binding = owner.registered_image(expected.dataset_id, expected.asset_id).map_err(WorkspaceThreadError::Dataset)?;
            if binding.image.sha256 != expected.base_image_digest
                || binding.annotation.as_ref().map(|file| &file.sha256) != expected.base_annotation_digest.as_ref()
                || binding.label_schema_fingerprint != expected.base_label_schema_digest
            {
                return Ok(false);
            }
            let changed = owner.connection.execute(
                "UPDATE review_sessions SET review_manifest_path=?1, source_task_id=?2, updated_at_ms=?3, row_revision=?4
                 WHERE review_id=?5 AND status='OPEN' AND row_revision=?6 AND review_manifest_path IS ?7",
                params![path, request.source_task_id.to_string(), now_ms, review_revision as i64,
                    request.review_id.to_string(), (review_revision - 1) as i64, old_path],
            ).map_err(review_db)?;
            Ok(changed == 1)
        }).map_err(ReviewDraftError::Storage)?;
        if !changed {
            return Err(ReviewDraftError::RevisionConflict);
        }
        Ok(DraftReceipt {
            review_id: request.review_id,
            draft_revision,
            draft_digest,
            review_revision,
        })
    }

    fn snapshot(&self, session: Uuid, review: Uuid) -> Result<DraftSnapshot, ReviewDraftError> {
        self.owner.workspace_action(session, move |owner| {
            let session = owner.get_review(review).map_err(WorkspaceThreadError::Review)?;
            owner.connection.query_row(
                "SELECT r.review_manifest_path, d.categories_json FROM review_sessions r JOIN datasets d USING(dataset_id)
                 WHERE r.review_id=?1",
                [review.to_string()],
                |row| Ok(DraftSnapshot {
                    root: owner.root().to_owned(),
                    session: session.clone(),
                    path: row.get(0)?,
                    categories: row.get(1)?,
                }),
            ).map_err(review_db)
        }).map_err(ReviewDraftError::Storage)
    }

    fn bind_tracks(
        &self,
        session: Uuid,
        review: &ReviewSession,
        document: &Value,
        selected: &[ReviewTrackSelection],
    ) -> Result<Vec<ReviewTrackBinding>, ReviewDraftError> {
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let invalid = || ReviewDraftError::InvalidTrackingSelection;
        let work = VideoAnnotationService::new(Arc::clone(&self.owner)).map_err(|_| invalid())?;
        let candidates = work
            .staging_tracking_candidates(session, review.dataset_id, review.asset_id)
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        let ArtifactOwner::Task(tracking_task_id) = candidates.reference.owner else {
            return Err(invalid());
        };
        let categories = annotation_categories(document);
        if selected.len() > categories.len() || selected.len() > candidates.observations.len() {
            return Err(invalid());
        }
        let track_categories: BTreeMap<_, _> = candidates
            .observations
            .iter()
            .filter_map(|candidate| {
                Some((
                    candidate.track_id,
                    candidate.category_id.parse::<u64>().ok()?,
                ))
            })
            .collect();
        let mut annotation_ids = BTreeSet::new();
        let mut track_ids = BTreeSet::new();
        let mut bindings = Vec::with_capacity(selected.len());
        for choice in selected {
            let category = categories
                .get(&choice.local_annotation_id)
                .ok_or_else(invalid)?;
            if track_categories.get(&choice.track_id) != Some(category)
                || !annotation_ids.insert(choice.local_annotation_id)
                || !track_ids.insert(choice.track_id)
            {
                return Err(invalid());
            }
            bindings.push(ReviewTrackBinding {
                local_annotation_id: choice.local_annotation_id,
                track_id: choice.track_id,
                tracking_task_id,
                tracking_attempt_id: candidates.tracking_attempt_id,
            });
        }
        bindings.sort_unstable_by_key(|binding| binding.local_annotation_id);
        Ok(bindings)
    }

    fn load(&self, snapshot: &DraftSnapshot) -> Result<Option<StoredDraft>, ReviewDraftError> {
        let Some(path) = &snapshot.path else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        open_scoped_file(&snapshot.root, Path::new(path))?
            .take(DRAFT_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        let current = &snapshot.session;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if bytes.len() as u64 > DRAFT_LIMIT || path != &draft_path(current.review_id, &digest) {
            return Err(ReviewDraftError::InvalidArtifact);
        }
        let document = self
            .contracts
            .validate(JsonProfile::ReviewDraft, &bytes)
            .map_err(ReviewDraftError::Contract)?;
        let artifact: DraftArtifact =
            serde_json::from_value(document).map_err(ReviewDraftError::Json)?;
        if artifact.review_id != current.review_id.to_string()
            || artifact.asset_id != current.asset_id.to_string()
            || artifact.image_digest != current.base_image_digest
            || artifact.base_annotation_digest != current.base_annotation_digest
            || artifact.base_label_schema_digest != current.base_label_schema_digest
        {
            return Err(ReviewDraftError::InvalidArtifact);
        }
        Ok(Some(StoredDraft {
            receipt: DraftReceipt {
                review_id: current.review_id,
                draft_revision: artifact
                    .draft_revision
                    .parse()
                    .map_err(|_| ReviewDraftError::InvalidArtifact)?,
                draft_digest: digest,
                review_revision: current.row_revision,
            },
            source_task_id: Uuid::parse_str(&artifact.source_task_id)
                .map_err(|_| ReviewDraftError::InvalidArtifact)?,
            coco_document: artifact.coco_document,
            track_bindings: artifact.track_bindings,
        }))
    }
}

fn annotation_categories(document: &Value) -> BTreeMap<u64, u64> {
    document["annotations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|annotation| {
            Some((
                annotation["id"].as_u64()?,
                annotation["category_id"].as_u64()?,
            ))
        })
        .collect()
}

fn retain_tracks(old: &StoredDraft, next: &Value) -> Vec<ReviewTrackBinding> {
    if old.track_bindings.is_empty() {
        return Vec::new();
    }
    let old_categories = annotation_categories(&old.coco_document);
    let next_categories = annotation_categories(next);
    old.track_bindings
        .iter()
        .filter(|binding| {
            old_categories
                .get(&binding.local_annotation_id)
                .is_some_and(|category| {
                    next_categories.get(&binding.local_annotation_id) == Some(category)
                })
        })
        .cloned()
        .collect()
}

pub(crate) fn draft_path(review: Uuid, digest: &str) -> String {
    format!("artifacts/reviews/{review}/{digest}.json")
}

fn review_db(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Review(ReviewStoreError::Database(error))
}
