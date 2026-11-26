//! Transactional registration of the reviewed, read-only per-image COCO plan.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::Path;

use rusqlite::{Transaction, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

use crate::import_preflight::{Annotation, ImportInput, ImportPreview, inspect_single_image_coco};
use crate::json::JsonContracts;
use crate::workspace::OpenedWorkspace;

#[derive(Debug)]
pub enum ImportRegistrationError {
    InputChanged,
    BlockedInput,
    InvalidDatasetName,
    Source(io::Error),
    Categories(serde_json::Error),
    Database(rusqlite::Error),
}

impl Display for ImportRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputChanged => write!(formatter, "Import inputs changed since preflight"),
            Self::BlockedInput => write!(formatter, "Resolve blocked import inputs first"),
            Self::InvalidDatasetName => write!(formatter, "Dataset name is empty"),
            Self::Source(error) => write!(formatter, "Import source: {error}"),
            Self::Categories(error) => write!(formatter, "Encode Dataset categories: {error}"),
            Self::Database(error) => write!(formatter, "Register import in SQLite: {error}"),
        }
    }
}

impl Error for ImportRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Categories(error) => Some(error),
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRegistration {
    pub dataset_id: String,
    pub asset_count: usize,
}

#[derive(Serialize)]
struct DatasetCategory<'a> {
    id: usize,
    name: &'a str,
}

pub(crate) struct PreparedImport {
    name: String,
    categories_json: String,
    assets: Vec<PreparedAsset>,
}

struct PreparedAsset {
    image_path: String,
    name: String,
    media_sha256: String,
    annotation: Option<PreparedAnnotation>,
}

struct PreparedAnnotation {
    path: String,
    sha256: String,
    local_image_id: i64,
}

pub(crate) fn prepare_registration(
    contracts: &JsonContracts,
    dataset_name: &str,
    preview: &ImportPreview,
) -> Result<PreparedImport, ImportRegistrationError> {
    if dataset_name.trim().is_empty() {
        return Err(ImportRegistrationError::InvalidDatasetName);
    }
    let selected: Vec<_> = preview
        .inputs
        .iter()
        .map(|input| input.image.clone())
        .collect();
    let current = inspect_single_image_coco(&preview.root, contracts, &selected)
        .map_err(ImportRegistrationError::Source)?;
    if &current != preview {
        return Err(ImportRegistrationError::InputChanged);
    }

    let mut names = BTreeSet::new();
    for input in &current.inputs {
        match &input.annotation {
            Annotation::Missing => {}
            Annotation::Reference { categories, .. } => {
                names.extend(categories.iter().map(|category| category.name.as_str()));
            }
            Annotation::Blocked(_) => return Err(ImportRegistrationError::BlockedInput),
        }
    }
    let categories: Vec<_> = names
        .into_iter()
        .enumerate()
        .map(|(index, name)| DatasetCategory {
            id: index + 1,
            name,
        })
        .collect();
    let categories_json =
        serde_json::to_string(&categories).map_err(ImportRegistrationError::Categories)?;
    let assets = current
        .inputs
        .into_iter()
        .map(|input| prepare_asset(&current.root, input))
        .collect::<Result<_, _>>()?;
    Ok(PreparedImport {
        name: dataset_name.trim().into(),
        categories_json,
        assets,
    })
}

impl OpenedWorkspace {
    /// Commit a new Dataset after the caller has presented and approved the
    /// preview. Store exact source bindings and leave every member unreviewed.
    pub fn register_external_coco(
        &mut self,
        contracts: &JsonContracts,
        dataset_name: &str,
        preview: &ImportPreview,
        now_ms: i64,
    ) -> Result<ImportRegistration, ImportRegistrationError> {
        self.commit_import(
            prepare_registration(contracts, dataset_name, preview)?,
            now_ms,
        )
    }

    pub(crate) fn commit_import(
        &mut self,
        prepared: PreparedImport,
        now_ms: i64,
    ) -> Result<ImportRegistration, ImportRegistrationError> {
        let dataset_id = Uuid::new_v4().to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ImportRegistrationError::Database)?;
        transaction.execute(
            "INSERT INTO datasets (dataset_id, name, categories_json, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![dataset_id, prepared.name, prepared.categories_json, now_ms],
        ).map_err(ImportRegistrationError::Database)?;
        let asset_count = prepared.assets.len();
        for asset in prepared.assets {
            register_asset(&transaction, &dataset_id, asset, now_ms)?;
        }
        transaction
            .commit()
            .map_err(ImportRegistrationError::Database)?;
        Ok(ImportRegistration {
            dataset_id,
            asset_count,
        })
    }
}

fn prepare_asset(
    root: &Path,
    input: ImportInput,
) -> Result<PreparedAsset, ImportRegistrationError> {
    let image_path = binding(root, &input.image)?;
    let name = input
        .image
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(ImportRegistrationError::InputChanged)?
        .to_owned();
    let media_sha256 = input
        .media_sha256
        .ok_or(ImportRegistrationError::BlockedInput)?;
    let annotation = match input.annotation {
        Annotation::Missing => None,
        Annotation::Reference {
            path,
            sha256,
            local_image_id,
            ..
        } => Some(PreparedAnnotation {
            path: binding(root, &path)?,
            sha256,
            local_image_id: i64::try_from(local_image_id)
                .map_err(|_| ImportRegistrationError::BlockedInput)?,
        }),
        Annotation::Blocked(_) => return Err(ImportRegistrationError::BlockedInput),
    };
    Ok(PreparedAsset {
        image_path,
        name,
        media_sha256,
        annotation,
    })
}

fn register_asset(
    transaction: &Transaction<'_>,
    dataset_id: &str,
    asset: PreparedAsset,
    now_ms: i64,
) -> Result<(), ImportRegistrationError> {
    let asset_id = Uuid::new_v4().to_string();
    let (annotation_path, format, origin, local_image_id, annotation_digest) =
        match asset.annotation {
            Some(annotation) => (
                Some(annotation.path),
                Some("COCO_IMAGE_BBOX"),
                "PREEXISTING",
                Some(annotation.local_image_id),
                Some(annotation.sha256),
            ),
            None => (None, None, "NONE", None, None),
        };
    transaction
        .execute(
            "INSERT INTO assets
            (asset_id, kind, location_kind, path, original_filename, file_origin,
             annotation_path, annotation_format, annotation_origin, annotation_local_image_id,
             media_digest, annotation_digest, created_at_ms, updated_at_ms)
         VALUES (?1, 'IMAGE', 'EXTERNAL', ?2, ?3, 'IMPORTED', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                asset_id,
                asset.image_path,
                asset.name,
                annotation_path,
                format,
                origin,
                local_image_id,
                asset.media_sha256,
                annotation_digest,
                now_ms
            ],
        )
        .map_err(ImportRegistrationError::Database)?;
    transaction
        .execute(
            "INSERT INTO dataset_assets (dataset_id, asset_id, added_at_ms) VALUES (?1, ?2, ?3)",
            params![dataset_id, asset_id, now_ms],
        )
        .map_err(ImportRegistrationError::Database)?;
    Ok(())
}

fn binding(root: &Path, relative: &Path) -> Result<String, ImportRegistrationError> {
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(ImportRegistrationError::Source)?;
    if !path.starts_with(root) {
        return Err(ImportRegistrationError::InputChanged);
    }
    path.into_os_string()
        .into_string()
        .map_err(|_| ImportRegistrationError::InputChanged)
}
