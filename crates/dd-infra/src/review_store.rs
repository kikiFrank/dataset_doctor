//! The Workspace database authority for review sessions and their registered baselines.

use std::error::Error;
use std::fmt::{self, Display};

use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::dataset_query::DatasetQueryError;
use crate::workspace::OpenedWorkspace;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewStatus {
    Open,
    Committing,
    Confirmed,
    Cancelled,
    Conflict,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSession {
    pub review_id: Uuid,
    pub asset_id: Uuid,
    pub dataset_id: Uuid,
    pub status: ReviewStatus,
    pub row_revision: u64,
    pub base_image_digest: String,
    pub base_annotation_digest: Option<String>,
    pub base_label_schema_digest: String,
}

#[derive(Debug)]
pub enum ReviewStoreError {
    Database(rusqlite::Error),
    Dataset(DatasetQueryError),
    UnknownReview,
    RevisionConflict,
    InvalidState,
    InvalidRecord,
}

impl Display for ReviewStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "Workspace review persistence: {error}"),
            Self::Dataset(error) => Display::fmt(error, formatter),
            Self::UnknownReview => formatter.write_str("Review does not exist in this Workspace"),
            Self::RevisionConflict => formatter.write_str("Review changed since it was read"),
            Self::InvalidState => formatter.write_str("Review status does not permit this action"),
            Self::InvalidRecord => {
                formatter.write_str("Workspace contains an invalid review record")
            }
        }
    }
}

impl Error for ReviewStoreError {}

impl From<rusqlite::Error> for ReviewStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

const REVIEW_COLUMNS: &str = "review_id, asset_id, dataset_id, status, row_revision,
    baseline_media_digest, baseline_annotation_digest, baseline_label_schema_digest";

impl OpenedWorkspace {
    pub(crate) fn open_review(
        &mut self,
        dataset: Uuid,
        asset: Uuid,
        now_ms: i64,
    ) -> Result<ReviewSession, ReviewStoreError> {
        let binding = self
            .registered_image(dataset, asset)
            .map_err(ReviewStoreError::Dataset)?;
        let transaction = self.connection.transaction()?;
        let existing = transaction.query_row(
            &format!("SELECT {REVIEW_COLUMNS} FROM review_sessions
                      WHERE asset_id=?1 AND dataset_id=?2 AND status IN ('OPEN','COMMITTING','CONFLICT')"),
            params![asset.to_string(), dataset.to_string()],
            decode,
        ).optional()?.transpose()?;
        if let Some(existing) = existing {
            return Ok(existing);
        }
        let review = ReviewSession {
            review_id: Uuid::new_v4(),
            asset_id: asset,
            dataset_id: dataset,
            status: ReviewStatus::Open,
            row_revision: 0,
            base_image_digest: binding.image.sha256,
            base_annotation_digest: binding.annotation.map(|file| file.sha256),
            base_label_schema_digest: binding.label_schema_fingerprint,
        };
        transaction.execute(
            "INSERT INTO review_sessions
                (review_id, asset_id, dataset_id, baseline_media_digest, baseline_annotation_digest,
                 baseline_label_schema_digest, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                review.review_id.to_string(),
                asset.to_string(),
                dataset.to_string(),
                review.base_image_digest,
                review.base_annotation_digest,
                review.base_label_schema_digest,
                now_ms
            ],
        )?;
        transaction.commit()?;
        Ok(review)
    }

    pub(crate) fn get_review(&self, review: Uuid) -> Result<ReviewSession, ReviewStoreError> {
        self.connection
            .query_row(
                &format!("SELECT {REVIEW_COLUMNS} FROM review_sessions WHERE review_id=?1"),
                [review.to_string()],
                decode,
            )
            .optional()?
            .ok_or(ReviewStoreError::UnknownReview)?
    }

    pub(crate) fn cancel_review(
        &mut self,
        review: Uuid,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ReviewSession, ReviewStoreError> {
        let current = self.get_review(review)?;
        if current.row_revision != expected_revision {
            return Err(ReviewStoreError::RevisionConflict);
        }
        if !matches!(current.status, ReviewStatus::Open | ReviewStatus::Conflict) {
            return Err(ReviewStoreError::InvalidState);
        }
        self.connection.execute(
            "UPDATE review_sessions SET status='CANCELLED', review_manifest_path=NULL,
                 row_revision=row_revision+1, updated_at_ms=?2 WHERE review_id=?1",
            params![review.to_string(), now_ms],
        )?;
        self.get_review(review)
    }
}

fn decode(row: &Row<'_>) -> rusqlite::Result<Result<ReviewSession, ReviewStoreError>> {
    let review_id: String = row.get(0)?;
    let asset_id: String = row.get(1)?;
    let dataset_id: String = row.get(2)?;
    let status: String = row.get(3)?;
    let status = match status.as_str() {
        "OPEN" => ReviewStatus::Open,
        "COMMITTING" => ReviewStatus::Committing,
        "CONFIRMED" => ReviewStatus::Confirmed,
        "CANCELLED" => ReviewStatus::Cancelled,
        "CONFLICT" => ReviewStatus::Conflict,
        "STALE" => ReviewStatus::Stale,
        _ => return Ok(Err(ReviewStoreError::InvalidRecord)),
    };
    let (Ok(review_id), Ok(asset_id), Ok(dataset_id)) = (
        Uuid::parse_str(&review_id),
        Uuid::parse_str(&asset_id),
        Uuid::parse_str(&dataset_id),
    ) else {
        return Ok(Err(ReviewStoreError::InvalidRecord));
    };
    let Ok(row_revision) = u64::try_from(row.get::<_, i64>(4)?) else {
        return Ok(Err(ReviewStoreError::InvalidRecord));
    };
    Ok(Ok(ReviewSession {
        review_id,
        asset_id,
        dataset_id,
        status,
        row_revision,
        base_image_digest: row.get(5)?,
        base_annotation_digest: row.get(6)?,
        base_label_schema_digest: row.get(7)?,
    }))
}
