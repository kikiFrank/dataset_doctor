use std::collections::{HashMap, HashSet};
use std::io::Read;

use rusqlite::OptionalExtension;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    ANNOTATION_LIMIT, CocoExportError, ExcludedAsset, ExclusionReason, IMAGE_LIMIT, hash_file,
    open_source,
};
use crate::dataset_query::{DatasetQueryError, RegisteredImage, RegisteredInputFile};
use crate::import_preflight::LocalCategory;
use crate::json::{JsonContracts, JsonProfile};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub(super) struct PreparedSelection {
    pub images: Vec<ExportableImage>,
    pub categories: Vec<LocalCategory>,
    pub categories_digest: String,
    pub exclusions: Vec<ExcludedAsset>,
}

pub(super) struct ExportableImage {
    pub asset: Uuid,
    pub media: RegisteredInputFile,
    pub annotation: RegisteredInputFile,
    pub document: Value,
    pub category_ids: HashMap<u64, u64>,
    pub final_review: String,
    pub confirmed_negative: bool,
}

struct Member {
    asset: Uuid,
    available: bool,
    active_review: bool,
    confirmation: Confirmation,
    source: Option<RegisteredImage>,
}

struct Confirmation {
    id: Option<String>,
    status: Option<String>,
    image_digest: Option<String>,
    annotation_digest: Option<String>,
    label_schema_digest: Option<String>,
    confirmed_negative: Option<bool>,
}

pub(super) fn collect(
    owner: &WorkspaceThread,
    contracts: &JsonContracts,
    session: Uuid,
    dataset: Uuid,
) -> Result<PreparedSelection, CocoExportError> {
    let (raw_categories, members) = owner.workspace_action(session, move |owner| {
        let dataset_id = dataset.to_string();
        let categories: String = owner.connection.query_row(
            "SELECT categories_json FROM datasets WHERE dataset_id=?1", [&dataset_id], |row| row.get(0),
        ).optional().map_err(database)?.ok_or(WorkspaceThreadError::Dataset(DatasetQueryError::UnknownDataset))?;
        let mut statement = owner.connection.prepare(
            "SELECT m.asset_id, a.availability,
                    EXISTS (SELECT 1 FROM review_sessions q WHERE q.asset_id=m.asset_id
                        AND q.status IN ('OPEN','COMMITTING','CONFLICT')),
                    r.review_id, r.status, r.confirmed_media_digest, r.confirmed_annotation_digest,
                    r.confirmed_label_schema_digest,
                    (SELECT json_extract(o.result_json,'$.confirmed_negative') FROM file_operations o
                        WHERE o.review_id=r.review_id AND o.operation_kind='REVIEW_COMMIT' AND o.status='COMMITTED'
                        ORDER BY o.completed_at_ms DESC LIMIT 1)
             FROM dataset_assets m JOIN assets a USING(asset_id)
             LEFT JOIN review_sessions r ON r.review_id=m.final_review_id
             WHERE m.dataset_id=?1 ORDER BY m.asset_id",
        ).map_err(database)?;
        let mut rows = statement.query([&dataset_id]).map_err(database)?;
        let mut members = Vec::new();
        while let Some(row) = rows.next().map_err(database)? {
            let asset_text: String = row.get(0).map_err(database)?;
            let asset = Uuid::parse_str(&asset_text).map_err(|_| WorkspaceThreadError::Dataset(DatasetQueryError::InvalidImageBinding))?;
            let availability: String = row.get(1).map_err(database)?;
            let active_review: bool = row.get(2).map_err(database)?;
            let confirmation = Confirmation {
                id: row.get(3).map_err(database)?,
                status: row.get(4).map_err(database)?,
                image_digest: row.get(5).map_err(database)?,
                annotation_digest: row.get(6).map_err(database)?,
                label_schema_digest: row.get(7).map_err(database)?,
                confirmed_negative: row.get(8).map_err(database)?,
            };
            let source = if availability == "ONLINE" && !active_review && confirmation.id.is_some() {
                match owner.registered_image(dataset, asset) {
                    Ok(binding) => Some(binding),
                    Err(error @ DatasetQueryError::Database(_)) => return Err(WorkspaceThreadError::Dataset(error)),
                    Err(_) => None,
                }
            } else {
                None
            };
            members.push(Member { asset, available: availability == "ONLINE", active_review, confirmation, source });
        }
        Ok((categories, members))
    }).map_err(CocoExportError::Storage)?;

    let categories_digest = format!("{:x}", Sha256::digest(raw_categories.as_bytes()));
    let categories: Vec<LocalCategory> =
        serde_json::from_str(&raw_categories).map_err(CocoExportError::Json)?;
    let category_index: HashMap<_, _> = categories
        .iter()
        .map(|category| (category.name.as_str(), category.id))
        .collect();
    let category_ids: HashSet<_> = categories.iter().map(|category| category.id).collect();
    if category_index.len() != categories.len()
        || category_ids.len() != categories.len()
        || categories
            .iter()
            .any(|item| item.name.is_empty() || item.id > 9_007_199_254_740_991)
    {
        return Err(CocoExportError::InvalidData);
    }
    let mut images = Vec::new();
    let mut exclusions = Vec::new();
    for member in members {
        let asset_id = member.asset;
        match confirmed(member, contracts, &category_index, &categories_digest) {
            Ok(image) => images.push(image),
            Err(reason) => exclusions.push(ExcludedAsset { asset_id, reason }),
        }
    }
    Ok(PreparedSelection {
        images,
        categories,
        categories_digest,
        exclusions,
    })
}

fn confirmed(
    member: Member,
    contracts: &JsonContracts,
    dataset_categories: &HashMap<&str, u64>,
    categories_digest: &str,
) -> Result<ExportableImage, ExclusionReason> {
    if member.active_review {
        return Err(ExclusionReason::ActiveReview);
    }
    if !member.available {
        return Err(ExclusionReason::Offline);
    }
    let review = member.confirmation;
    let final_review = review.id.ok_or(ExclusionReason::Unconfirmed)?;
    if review.status.as_deref() != Some("CONFIRMED")
        || review.label_schema_digest.as_deref() != Some(categories_digest)
    {
        return Err(ExclusionReason::InputChanged);
    }
    let source = member.source.ok_or(ExclusionReason::InputChanged)?;
    let annotation = source.annotation.ok_or(ExclusionReason::InputChanged)?;
    if review.image_digest.as_deref() != Some(source.image.sha256.as_str())
        || review.annotation_digest.as_deref() != Some(annotation.sha256.as_str())
        || hash_file(&source.image.path, IMAGE_LIMIT).map_err(file_error)? != source.image.sha256
    {
        return Err(ExclusionReason::InputChanged);
    }
    let mut bytes = Vec::new();
    open_source(&annotation.path)
        .map_err(file_error)?
        .take(ANNOTATION_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(file_error)?;
    if bytes.len() as u64 > ANNOTATION_LIMIT
        || format!("{:x}", Sha256::digest(&bytes)) != annotation.sha256
    {
        return Err(ExclusionReason::InputChanged);
    }
    let document = contracts
        .validate(JsonProfile::CocoSingleImage, &bytes)
        .map_err(|_| ExclusionReason::InputChanged)?;
    let labels = document["annotations"]
        .as_array()
        .ok_or(ExclusionReason::InputChanged)?;
    let confirmed_negative = review
        .confirmed_negative
        .ok_or(ExclusionReason::Unconfirmed)?;
    if labels.is_empty() != confirmed_negative {
        return Err(ExclusionReason::Unconfirmed);
    }
    let local_image = document["images"][0]["id"]
        .as_u64()
        .ok_or(ExclusionReason::InputChanged)?;
    let categories = document["categories"]
        .as_array()
        .ok_or(ExclusionReason::InputChanged)?;
    let mut category_ids = HashMap::with_capacity(categories.len());
    for category in categories {
        let id = category["id"]
            .as_u64()
            .ok_or(ExclusionReason::InputChanged)?;
        let name = category["name"]
            .as_str()
            .ok_or(ExclusionReason::InputChanged)?;
        let mapped_id = dataset_categories
            .get(name)
            .ok_or(ExclusionReason::InputChanged)?;
        if category_ids.insert(id, *mapped_id).is_some() {
            return Err(ExclusionReason::InputChanged);
        }
    }
    let mut annotations = HashSet::with_capacity(labels.len());
    for label in labels {
        let id = label["id"].as_u64().ok_or(ExclusionReason::InputChanged)?;
        let category = label["category_id"]
            .as_u64()
            .ok_or(ExclusionReason::InputChanged)?;
        if label["image_id"].as_u64() != Some(local_image)
            || !annotations.insert(id)
            || !category_ids.contains_key(&category)
        {
            return Err(ExclusionReason::InputChanged);
        }
    }
    Ok(ExportableImage {
        asset: member.asset,
        media: source.image,
        annotation,
        document,
        category_ids,
        final_review,
        confirmed_negative,
    })
}

fn file_error(error: std::io::Error) -> ExclusionReason {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            ExclusionReason::Offline
        }
        _ => ExclusionReason::InputChanged,
    }
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Dataset(DatasetQueryError::Database(error))
}
