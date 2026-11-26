//! Bounded read-only view of a registered image and its current formal COCO.

use std::{
    error::Error,
    fmt::{self, Display},
    io::{self, Read},
    sync::Arc,
};

use rusqlite::OptionalExtension;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dataset_query::{AssetAnnotationState, DatasetQueryError};
use crate::json::{ContractError, JsonContracts, JsonProfile};
use crate::paths::open_registered_file;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

#[derive(Debug)]
pub struct AnnotationView {
    pub asset_id: Uuid,
    pub image_digest: String,
    pub annotation_digest: Option<String>,
    pub formal_coco: Option<Value>,
    pub annotation_state: AssetAnnotationState,
    pub active_review: Option<Uuid>,
}

#[derive(Debug)]
pub enum AnnotationReadError {
    Setup(Box<dyn Error + Send + Sync>),
    Storage(WorkspaceThreadError),
    Io(io::Error),
    Contract(ContractError),
    InvalidRecord,
}

impl Display for AnnotationReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => write!(f, "Initialize annotation reader: {error}"),
            Self::Storage(error) => Display::fmt(error, f),
            Self::Io(error) => write!(f, "Read registered annotation: {error}"),
            Self::Contract(error) => Display::fmt(error, f),
            Self::InvalidRecord => f.write_str("Invalid registered annotation or review"),
        }
    }
}
impl Error for AnnotationReadError {}

pub struct AnnotationReader {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

impl AnnotationReader {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, AnnotationReadError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(AnnotationReadError::Setup)?,
        })
    }

    pub fn read(&self, session: Uuid, asset: Uuid) -> Result<AnnotationView, AnnotationReadError> {
        let (binding, active, confirmed) = self.owner.workspace_action(session, move |owner| {
            let asset_id = asset.to_string();
            let dataset = owner.image_dataset(asset).map_err(WorkspaceThreadError::Dataset)?;
            let binding = owner.registered_image(dataset, asset).map_err(WorkspaceThreadError::Dataset)?;
            let active: Option<String> = owner.connection.query_row(
                "SELECT review_id FROM review_sessions WHERE asset_id=?1 AND status IN ('OPEN','COMMITTING','CONFLICT')",
                [&asset_id], |row| row.get(0),
            ).optional().map_err(database)?;
            let confirmed: Option<(String, String, String, String)> = owner.connection.query_row(
                "SELECT r.status, r.confirmed_media_digest, r.confirmed_annotation_digest, r.confirmed_label_schema_digest
                 FROM review_sessions r WHERE r.asset_id=?1 AND r.dataset_id=?2 AND r.review_id=COALESCE(
                     (SELECT final_review_id FROM dataset_assets WHERE asset_id=?1 AND dataset_id=?2),
                     (SELECT review_id FROM staging_items WHERE asset_id=?1 AND target_dataset_id=?2)
                 )",
                rusqlite::params![asset_id,dataset.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).optional().map_err(database)?;
            Ok((binding, active, confirmed))
        }).map_err(AnnotationReadError::Storage)?;
        let mut image_sha = Sha256::new();
        let length = io::copy(
            &mut open_registered_file(&binding.image.path)
                .map_err(AnnotationReadError::Io)?
                .take(256 * 1024 * 1024 + 1),
            &mut image_sha,
        )
        .map_err(AnnotationReadError::Io)?;
        if length > 256 * 1024 * 1024 {
            return Err(AnnotationReadError::InvalidRecord);
        }
        let image_digest = format!("{:x}", image_sha.finalize());
        let (formal_coco, annotation_digest) = if let Some(annotation) = &binding.annotation {
            let mut bytes = Vec::new();
            open_registered_file(&annotation.path)
                .map_err(AnnotationReadError::Io)?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(AnnotationReadError::Io)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(AnnotationReadError::InvalidRecord);
            }
            let digest = format!("{:x}", Sha256::digest(&bytes));
            let document = self
                .contracts
                .validate(JsonProfile::CocoSingleImage, &bytes)
                .map_err(AnnotationReadError::Contract)?;
            (Some(document), Some(digest))
        } else {
            (None, None)
        };
        let unchanged = image_digest == binding.image.sha256
            && annotation_digest.as_ref() == binding.annotation.as_ref().map(|file| &file.sha256);
        let annotation_state = match (unchanged, &confirmed, &formal_coco) {
            (false, _, _) => AssetAnnotationState::Stale,
            (true, None, None) => AssetAnnotationState::Unannotated,
            (true, None, Some(_)) => AssetAnnotationState::Unreviewed,
            (true, Some((status, image, annotation, schema)), Some(document))
                if status == "CONFIRMED"
                    && image == &image_digest
                    && Some(annotation) == annotation_digest.as_ref()
                    && schema == &binding.label_schema_fingerprint =>
            {
                if document["annotations"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
                {
                    AssetAnnotationState::ConfirmedNegative
                } else {
                    AssetAnnotationState::ConfirmedPositive
                }
            }
            _ => AssetAnnotationState::Stale,
        };
        let active_review = active
            .map(|id| Uuid::parse_str(&id).map_err(|_| AnnotationReadError::InvalidRecord))
            .transpose()?;
        Ok(AnnotationView {
            asset_id: asset,
            image_digest,
            annotation_digest,
            formal_coco,
            annotation_state,
            active_review,
        })
    }
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Dataset(DatasetQueryError::Database(error))
}
