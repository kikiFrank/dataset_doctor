//! Bounded Workspace snapshots for Dataset and Asset browsing.

use std::error::Error;
use std::fmt::{self, Display};

use rusqlite::{Row, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::sqlite::{decode_uuid, row_uuid};
use crate::workspace::OpenedWorkspace;

const MAX_PAGE: u16 = 200;

#[derive(Debug)]
pub enum DatasetQueryError {
    InvalidPageSize,
    UnknownDataset,
    Database(rusqlite::Error),
}

impl Display for DatasetQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageSize => {
                write!(formatter, "Page size must be between 1 and {MAX_PAGE}")
            }
            Self::UnknownDataset => formatter.write_str("Dataset is not in the current Workspace"),
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

impl OpenedWorkspace {
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
