//! Bounded Workspace snapshots for Dataset and Asset browsing.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;

use rusqlite::{OptionalExtension, Row, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::paths::is_portable_relative_file;
use crate::sqlite::{decode_uuid, row_uuid};
use crate::workspace::OpenedWorkspace;

const MAX_PAGE: u16 = 200;

#[derive(Debug)]
pub enum DatasetQueryError {
    InvalidPageSize,
    UnknownDataset,
    UnknownAsset,
    UnavailableImage,
    InvalidImageBinding,
    Database(rusqlite::Error),
}

impl Display for DatasetQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageSize => {
                write!(formatter, "Page size must be between 1 and {MAX_PAGE}")
            }
            Self::UnknownDataset => formatter.write_str("Dataset is not in the current Workspace"),
            Self::UnknownAsset => formatter.write_str("Asset is not a member of this Dataset"),
            Self::UnavailableImage => formatter.write_str("Asset is not an available image"),
            Self::InvalidImageBinding => {
                formatter.write_str("Registered image or annotation binding is incomplete")
            }
            Self::Database(error) => write!(formatter, "Read Workspace Dataset entries: {error}"),
        }
    }
}

impl Error for DatasetQueryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for DatasetQueryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Debug)]
pub struct DatasetPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug)]
pub struct DatasetSummary {
    pub dataset_id: Uuid,
    pub name: String,
    pub asset_count: u64,
    pub unconfirmed_count: u64,
    pub row_revision: u64,
    pub schema_fingerprint: String,
    pub updated_at_ms: i64,
}

#[derive(Debug)]
pub struct AssetSummary {
    pub asset_id: Uuid,
    pub filename: String,
    pub kind: String,
    pub availability: String,
    pub has_annotation: bool,
    pub final_review_id: Option<Uuid>,
}

#[derive(Debug)]
pub struct RegisteredInputFile {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug)]
pub struct RegisteredImage {
    pub asset_id: Uuid,
    pub dataset_id: Uuid,
    pub image: RegisteredInputFile,
    pub annotation: Option<RegisteredInputFile>,
    pub label_schema_fingerprint: String,
}

struct ImageRow {
    kind: String,
    availability: String,
    location: String,
    path: String,
    media_digest: Option<String>,
    annotation_path: Option<String>,
    annotation_format: Option<String>,
    annotation_digest: Option<String>,
    categories: String,
}

impl OpenedWorkspace {
    pub(crate) fn registered_image(
        &self,
        dataset_id: Uuid,
        asset_id: Uuid,
    ) -> Result<RegisteredImage, DatasetQueryError> {
        let row = self
            .connection
            .query_row(
                "SELECT a.kind, a.availability, a.location_kind, a.path, a.media_digest,
                    a.annotation_path, a.annotation_format, a.annotation_digest, d.categories_json
             FROM dataset_assets m JOIN assets a USING (asset_id) JOIN datasets d USING (dataset_id)
             WHERE m.dataset_id=?1 AND m.asset_id=?2",
                params![dataset_id.to_string(), asset_id.to_string()],
                |row| {
                    Ok(ImageRow {
                        kind: row.get(0)?,
                        availability: row.get(1)?,
                        location: row.get(2)?,
                        path: row.get(3)?,
                        media_digest: row.get(4)?,
                        annotation_path: row.get(5)?,
                        annotation_format: row.get(6)?,
                        annotation_digest: row.get(7)?,
                        categories: row.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or(DatasetQueryError::UnknownAsset)?;
        if row.kind != "IMAGE" || row.availability != "ONLINE" {
            return Err(DatasetQueryError::UnavailableImage);
        }
        let image = self.registered_file(&row.location, row.path, row.media_digest)?;
        let annotation = match (
            row.annotation_path,
            row.annotation_format.as_deref(),
            row.annotation_digest,
        ) {
            (None, None, None) => None,
            (Some(path), Some("COCO_IMAGE_BBOX"), digest) => {
                Some(self.registered_file(&row.location, path, digest)?)
            }
            _ => return Err(DatasetQueryError::InvalidImageBinding),
        };
        Ok(RegisteredImage {
            asset_id,
            dataset_id,
            image,
            annotation,
            label_schema_fingerprint: format!("{:x}", Sha256::digest(row.categories.as_bytes())),
        })
    }

    fn registered_file(
        &self,
        location: &str,
        path: String,
        digest: Option<String>,
    ) -> Result<RegisteredInputFile, DatasetQueryError> {
        let path = PathBuf::from(path);
        let path = match location {
            "EXTERNAL" if path.is_absolute() => path,
            "WORKSPACE" if is_portable_relative_file(&path) => self.root().join(path),
            _ => return Err(DatasetQueryError::InvalidImageBinding),
        };
        let sha256 = digest
            .filter(|encoded| {
                encoded.len() == 64
                    && encoded
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
            .ok_or(DatasetQueryError::InvalidImageBinding)?;
        Ok(RegisteredInputFile { path, sha256 })
    }

    pub(crate) fn list_datasets(
        &self,
        after: Option<Uuid>,
        size: u16,
    ) -> Result<DatasetPage<DatasetSummary>, DatasetQueryError> {
        let limit = checked_limit(size)?;
        let mut statement = self.connection.prepare(
            "SELECT d.dataset_id, d.name, d.row_revision, d.categories_json, d.updated_at_ms,
                    (SELECT count(*) FROM dataset_assets m WHERE m.dataset_id=d.dataset_id),
                    (SELECT count(*) FROM dataset_assets m WHERE m.dataset_id=d.dataset_id AND m.final_review_id IS NULL)
             FROM datasets d WHERE ?1 IS NULL OR d.dataset_id > ?1
             ORDER BY d.dataset_id LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after.map(|id| id.to_string()), limit], |row| {
                let categories: String = row.get(3)?;
                Ok(DatasetSummary {
                    dataset_id: row_uuid(row, 0)?,
                    name: row.get(1)?,
                    row_revision: unsigned(row, 2)?,
                    schema_fingerprint: format!("{:x}", Sha256::digest(categories.as_bytes())),
                    updated_at_ms: row.get(4)?,
                    asset_count: unsigned(row, 5)?,
                    unconfirmed_count: unsigned(row, 6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(page(rows, size, |item| item.dataset_id))
    }

    pub(crate) fn list_assets(
        &self,
        dataset: Uuid,
        after: Option<Uuid>,
        size: u16,
    ) -> Result<DatasetPage<AssetSummary>, DatasetQueryError> {
        let limit = checked_limit(size)?;
        let dataset = dataset.to_string();
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM datasets WHERE dataset_id=?1)",
            [&dataset],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(DatasetQueryError::UnknownDataset);
        }
        let mut statement = self.connection.prepare(
            "SELECT a.asset_id, a.original_filename, a.kind, a.availability, a.annotation_path IS NOT NULL, m.final_review_id
             FROM dataset_assets m JOIN assets a ON a.asset_id=m.asset_id
             WHERE m.dataset_id=?1 AND (?2 IS NULL OR m.asset_id > ?2)
             ORDER BY m.asset_id LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![dataset, after.map(|id| id.to_string()), limit],
                |row| {
                    let final_review: Option<String> = row.get(5)?;
                    Ok(AssetSummary {
                        asset_id: row_uuid(row, 0)?,
                        filename: row.get(1)?,
                        kind: row.get(2)?,
                        availability: row.get(3)?,
                        has_annotation: row.get(4)?,
                        final_review_id: final_review
                            .as_deref()
                            .map(|value| decode_uuid(value, 5))
                            .transpose()?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(page(rows, size, |item| item.asset_id))
    }
}

fn checked_limit(size: u16) -> Result<i64, DatasetQueryError> {
    if !(1..=MAX_PAGE).contains(&size) {
        return Err(DatasetQueryError::InvalidPageSize);
    }
    Ok(i64::from(size) + 1)
}

fn page<T>(mut items: Vec<T>, size: u16, id: impl FnOnce(&T) -> Uuid) -> DatasetPage<T> {
    let more = items.len() > usize::from(size);
    items.truncate(usize::from(size));
    let next_cursor = if more { items.last().map(id) } else { None };
    DatasetPage { items, next_cursor }
}

fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
