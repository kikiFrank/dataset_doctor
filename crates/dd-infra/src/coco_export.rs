//! Package current, explicitly confirmed single-image annotations as standard COCO.

mod selection;

use std::error::Error;
use std::fmt::{self, Display};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use self::selection::{ExportableImage, PreparedSelection, collect};
use crate::json::JsonContracts;
use crate::paths::open_registered_file;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const IMAGE_LIMIT: u64 = 256 * 1024 * 1024;
const ANNOTATION_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CocoExportPolicy {
    Strict,
    ExcludeIneligible,
}

#[derive(Clone, Debug)]
pub struct CocoExportRequest {
    pub dataset_id: Uuid,
    pub destination_grant_id: Uuid,
    pub policy: CocoExportPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExclusionReason {
    Unconfirmed,
    ActiveReview,
    InputChanged,
    Offline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcludedAsset {
    pub asset_id: Uuid,
    pub reason: ExclusionReason,
}

#[derive(Debug)]
pub struct CocoExportReceipt {
    pub package: PathBuf,
    pub image_count: usize,
    pub annotation_count: usize,
    pub exclusions: Vec<ExcludedAsset>,
}

#[derive(Debug)]
pub enum CocoExportError {
    Setup(Box<dyn Error + Send + Sync>),
    Storage(WorkspaceThreadError),
    Json(serde_json::Error),
    Io(io::Error),
    Ineligible(Vec<ExcludedAsset>),
    NoEligibleImages,
    InvalidData,
    InputChanged,
}

impl Display for CocoExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "Initialize COCO export schemas: {error}"),
            Self::Storage(error) => Display::fmt(error, formatter),
            Self::Json(error) => write!(formatter, "Encode COCO export: {error}"),
            Self::Io(error) => write!(formatter, "Read or write COCO package: {error}"),
            Self::Ineligible(assets) => write!(
                formatter,
                "{} Dataset members are not eligible for export",
                assets.len()
            ),
            Self::NoEligibleImages => {
                formatter.write_str("Dataset has no confirmed images to export")
            }
            Self::InvalidData => formatter.write_str("Dataset contains invalid COCO export data"),
            Self::InputChanged => {
                formatter.write_str("An export input or destination changed during export")
            }
        }
    }
}

impl Error for CocoExportError {}

impl From<io::Error> for CocoExportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct CocoExportService {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

impl CocoExportService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, CocoExportError> {
        let contracts = JsonContracts::new().map_err(CocoExportError::Setup)?;
        Ok(Self { owner, contracts })
    }

    pub fn eligibility(
        &self,
        session: Uuid,
        dataset: Uuid,
    ) -> Result<Vec<ExcludedAsset>, CocoExportError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(CocoExportError::Storage)?;
        Ok(collect(&self.owner, &self.contracts, session, dataset)?.exclusions)
    }

    /// Stage privately in the selected parent and publish one complete package.
    pub fn export(
        &self,
        session: Uuid,
        request: CocoExportRequest,
    ) -> Result<CocoExportReceipt, CocoExportError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(CocoExportError::Storage)?;
        let destination = self
            .owner
            .consume_export_directory(session, request.destination_grant_id)
            .map_err(CocoExportError::Storage)?;
        let selection = collect(&self.owner, &self.contracts, session, request.dataset_id)?;
        if request.policy == CocoExportPolicy::Strict && !selection.exclusions.is_empty() {
            return Err(CocoExportError::Ineligible(selection.exclusions));
        }
        if selection.images.is_empty() {
            return Err(CocoExportError::NoEligibleImages);
        }
        if !destination.is_dir() || destination.canonicalize()? != destination {
            return Err(CocoExportError::InputChanged);
        }
        let stage = tempfile::Builder::new()
            .prefix(".datasetdoctor-export-")
            .tempdir_in(&destination)?;
        let (annotations, index) = assemble(stage.path(), request.dataset_id, &selection)?;
        write_json(stage.path().join("annotations.json"), &annotations)?;
        write_json(stage.path().join("manifest.json"), &index)?;
        // The source can be edited by other programs even while Core mutations
        // are gated. Confirm the source hashes once more before publication.
        for image in &selection.images {
            if hash_file(&image.media.path, IMAGE_LIMIT)? != image.media.sha256
                || hash_file(&image.annotation.path, ANNOTATION_LIMIT)? != image.annotation.sha256
            {
                return Err(CocoExportError::InputChanged);
            }
        }
        if destination.canonicalize()? != destination {
            return Err(CocoExportError::InputChanged);
        }
        let package = destination.join(format!("dataset-{}", Uuid::new_v4()));
        if package.try_exists()? {
            return Err(CocoExportError::InputChanged);
        }
        fs::rename(stage.path(), &package)?;
        Ok(CocoExportReceipt {
            package,
            image_count: selection.images.len(),
            annotation_count: annotations["annotations"]
                .as_array()
                .ok_or(CocoExportError::InvalidData)?
                .len(),
            exclusions: selection.exclusions,
        })
    }
}

fn assemble(
    directory: &Path,
    dataset: Uuid,
    selection: &PreparedSelection,
) -> Result<(Value, Value), CocoExportError> {
    fs::create_dir(directory.join("images"))?;
    let mut images = Vec::new();
    let mut annotations = Vec::new();
    let mut image_mapping = Vec::new();
    let mut annotation_mapping = Vec::new();
    for sample in &selection.images {
        let asset = sample.asset.to_string();
        let image_id = next_coco_id(images.len())?;
        let local_image = &sample.document["images"][0];
        let extension = sample
            .media
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|extension| matches!(extension.as_str(), "jpg" | "jpeg" | "png"))
            .ok_or(CocoExportError::InvalidData)?;
        let relative = format!("images/{image_id:06}.{extension}");
        copy_image(sample, &directory.join(&relative))?;
        images.push(json!({
            "id": image_id, "file_name": relative, "width": local_image["width"], "height": local_image["height"]
        }));
        image_mapping.push(json!({
            "asset_id": asset, "local_image_id": local_image["id"], "image_id": image_id,
            "path": relative, "image_digest": sample.media.sha256, "annotation_digest": sample.annotation.sha256,
            "final_review_id": sample.final_review, "confirmed_negative": sample.confirmed_negative
        }));
        for value in sample.document["annotations"]
            .as_array()
            .ok_or(CocoExportError::InvalidData)?
        {
            let local_id = value["category_id"]
                .as_u64()
                .ok_or(CocoExportError::InvalidData)?;
            let mapped = sample
                .category_ids
                .get(&local_id)
                .ok_or(CocoExportError::InvalidData)?;
            let annotation_id = next_coco_id(annotations.len())?;
            let mut exported = value.clone();
            exported["id"] = json!(annotation_id);
            exported["image_id"] = json!(image_id);
            exported["category_id"] = json!(mapped);
            annotations.push(exported);
            annotation_mapping.push(json!({"asset_id": asset, "local_annotation_id": value["id"], "annotation_id": annotation_id}));
        }
    }
    let categories: Vec<_> = selection
        .categories
        .iter()
        .map(|category| json!({"id": category.id, "name": category.name}))
        .collect();
    let excluded: Vec<_> = selection
        .exclusions
        .iter()
        .map(|item| json!({"asset_id": item.asset_id.to_string(), "reason": item.reason}))
        .collect();
    Ok((
        json!({"images": images, "annotations": annotations, "categories": categories}),
        json!({"contract_version": 1, "dataset_id": dataset.to_string(), "label_schema_digest": selection.categories_digest,
            "images": image_mapping, "annotations": annotation_mapping, "excluded": excluded}),
    ))
}

fn next_coco_id(current: usize) -> Result<u64, CocoExportError> {
    u64::try_from(current)
        .ok()
        .and_then(|value| value.checked_add(1))
        .filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or(CocoExportError::InvalidData)
}

fn copy_image(sample: &ExportableImage, path: &Path) -> Result<(), CocoExportError> {
    let source = open_registered_file(&sample.media.path)?;
    let mut target = File::create_new(path)?;
    if io::copy(&mut source.take(IMAGE_LIMIT + 1), &mut target)? > IMAGE_LIMIT {
        return Err(CocoExportError::InputChanged);
    }
    target.sync_all()?;
    drop(target);
    let parent = path
        .parent()
        .ok_or(CocoExportError::InvalidData)?
        .canonicalize()?;
    let staged = parent.join(path.file_name().ok_or(CocoExportError::InvalidData)?);
    if hash_file(&staged, IMAGE_LIMIT)? != sample.media.sha256 {
        return Err(CocoExportError::InputChanged);
    }
    Ok(())
}

fn hash_file(path: &Path, limit: u64) -> io::Result<String> {
    let mut sha = Sha256::new();
    if io::copy(&mut open_registered_file(path)?.take(limit + 1), &mut sha)? > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Source exceeds the export size limit",
        ));
    }
    Ok(format!("{:x}", sha.finalize()))
}

fn write_json(path: PathBuf, value: &Value) -> Result<(), CocoExportError> {
    let mut file = File::create_new(path)?;
    serde_json::to_writer(&mut file, value).map_err(CocoExportError::Json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
