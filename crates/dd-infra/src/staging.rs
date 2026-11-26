//! Read-only paging of Staging, with status derived from the current files and records.

use std::path::Path;

use rusqlite::{OptionalExtension, Row, params};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_reference::ArtifactReference;
use crate::dataset_query::{DatasetPage, DatasetQueryError, RegisteredInputFile, checked_limit};
use crate::sqlite::row_uuid;
#[cfg(feature = "execution-rpc")]
use crate::staging_report::{self, StoredReport};
use crate::task_store::parse_digest;
use crate::workspace::OpenedWorkspace;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const SCAN_LIMIT: i64 = 200;
const IMAGE_LIMIT: u64 = 256 * 1024 * 1024;
const ANNOTATION_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagingState {
    Offline,
    Conflict,
    Accepting,
    Checking,
    NeedsAnnotation,
    NeedsReview,
    NeedsCheck,
    ReadyForAcceptance,
}

#[derive(Clone, Copy)]
pub struct StagingQuery {
    pub target_dataset_id: Option<Uuid>,
    pub after: Option<Uuid>,
    pub status: Option<StagingState>,
    pub page_size: u16,
}

#[derive(Debug)]
pub struct StagingItem {
    pub asset_id: Uuid,
    pub target_dataset_id: Option<Uuid>,
    pub display_name: String,
    pub state: StagingState,
    pub review_state: Option<String>,
    pub source_frame: Option<StagingFrameSource>,
    pub current_report_ref: Option<ArtifactReference>,
    pub blocking_reasons: Vec<&'static str>,
}

#[derive(Debug)]
pub struct StagingFrameSource {
    pub source_video_id: Uuid,
    pub source_digest: String,
    pub stream_index: u32,
    pub frame_index: u64,
    pub pts_us: Option<i64>,
    pub extraction_task_id: Option<Uuid>,
}

struct StagingRow {
    asset: Uuid,
    dataset: Option<Uuid>,
    name: String,
    kind: String,
    availability: String,
    location: String,
    path: String,
    media_digest: Option<String>,
    annotation_path: Option<String>,
    annotation_digest: Option<String>,
    annotation_format: Option<String>,
    active_review: Option<String>,
    final_review: Option<String>,
    confirmed_media: Option<String>,
    confirmed_annotation: Option<String>,
    confirmed_schema: Option<String>,
    categories: Option<String>,
    source_video_id: Option<Uuid>,
    source_metadata: String,
    checking: bool,
    #[cfg(feature = "execution-rpc")]
    report: Option<StoredReport>,
}

impl StagingRow {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            asset: row_uuid(row, 0)?,
            dataset: optional_uuid(row, 1)?,
            name: row.get(2)?,
            kind: row.get(3)?,
            availability: row.get(4)?,
            location: row.get(5)?,
            path: row.get(6)?,
            media_digest: row.get(7)?,
            annotation_path: row.get(8)?,
            annotation_digest: row.get(9)?,
            annotation_format: row.get(10)?,
            active_review: row.get(11)?,
            final_review: row.get(12)?,
            confirmed_media: row.get(13)?,
            confirmed_annotation: row.get(14)?,
            confirmed_schema: row.get(15)?,
            categories: row.get(16)?,
            source_video_id: optional_uuid(row, 17)?,
            source_metadata: row.get(18)?,
            checking: row.get(19)?,
            #[cfg(feature = "execution-rpc")]
            report: StoredReport::read(row, 20).ok(),
        })
    }
}

struct StagingCandidate {
    row: StagingRow,
    media: Result<RegisteredInputFile, DatasetQueryError>,
    annotation: Result<Option<RegisteredInputFile>, DatasetQueryError>,
}

impl WorkspaceThread {
    pub fn list_staging(
        &self,
        session: Uuid,
        query: StagingQuery,
    ) -> Result<DatasetPage<StagingItem>, WorkspaceThreadError> {
        checked_limit(query.page_size).map_err(WorkspaceThreadError::Dataset)?;
        let scan = if query.status.is_some() {
            SCAN_LIMIT
        } else {
            i64::from(query.page_size)
        };
        let (root, rows) = self.workspace_action(session, move |workspace| {
            let rows = workspace
                .scan_staging(session, query, scan + 1)
                .map_err(WorkspaceThreadError::Dataset)?;
            Ok((workspace.root().to_owned(), rows))
        })?;
        let more = rows.len() > scan as usize;
        let mut candidates = rows.into_iter().take(scan as usize).peekable();
        let mut items = Vec::new();
        let mut last_examined = None;
        while let Some(candidate) = candidates.next() {
            last_examined = Some(candidate.row.asset);
            let item = candidate.view(&root);
            if query.status.is_none_or(|status| status == item.state) {
                items.push(item);
            }
            if items.len() == usize::from(query.page_size) {
                break;
            }
        }
        let next_cursor = if more || candidates.peek().is_some() {
            last_examined
        } else {
            None
        };
        Ok(DatasetPage { items, next_cursor })
    }
}

impl OpenedWorkspace {
    fn scan_staging(
        &self,
        session: Uuid,
        query: StagingQuery,
        limit: i64,
    ) -> Result<Vec<StagingCandidate>, DatasetQueryError> {
        if let Some(dataset) = query.target_dataset_id {
            self.connection
                .query_row(
                    "SELECT 1 FROM datasets WHERE dataset_id=?1",
                    [dataset.to_string()],
                    |_| Ok(()),
                )
                .optional()?
                .ok_or(DatasetQueryError::UnknownDataset)?;
        }
        #[cfg(feature = "execution-rpc")]
        let report_capability = staging_report::CAPABILITY;
        #[cfg(not(feature = "execution-rpc"))]
        let report_capability = "";
        let mut statement = self.connection.prepare(
            "SELECT a.asset_id,s.target_dataset_id,a.original_filename,a.kind,a.availability,a.location_kind,a.path,
                    a.media_digest,a.annotation_path,a.annotation_digest,a.annotation_format,
                    active.status,final.status,final.confirmed_media_digest,final.confirmed_annotation_digest,
                    final.confirmed_label_schema_digest,d.categories_json,a.source_video_id,a.source_metadata_json,
                    COALESCE(t.operation_kind='BUILD_REPORT' AND t.dataset_id=s.target_dataset_id
                        AND t.workspace_session_id=?3 AND t.status IN ('QUEUED','DISPATCHING','RUNNING'),0),
                    CASE WHEN t.operation_kind='BUILD_REPORT' AND t.dataset_id=s.target_dataset_id
                        AND t.capability_id=?5 AND t.status='SUCCEEDED' THEN t.task_id END,
                    t.output_manifest_path,t.input_fingerprint,t.dataset_id,
                    json_extract(t.execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
             FROM staging_items s JOIN assets a USING(asset_id)
             LEFT JOIN datasets d ON d.dataset_id=s.target_dataset_id
             LEFT JOIN review_sessions active ON active.asset_id=s.asset_id AND active.dataset_id=s.target_dataset_id
                  AND active.status IN ('OPEN','COMMITTING','CONFLICT')
             LEFT JOIN review_sessions final ON final.review_id=s.review_id
                  AND final.asset_id=s.asset_id AND final.dataset_id=s.target_dataset_id
             LEFT JOIN tasks t ON t.task_id=s.check_task_id
             WHERE (?1 IS NULL OR s.target_dataset_id=?1) AND (?2 IS NULL OR s.asset_id>?2)
             ORDER BY s.asset_id LIMIT ?4",
        )?;
        statement
            .query_map(
                params![
                    query.target_dataset_id.map(|id| id.to_string()),
                    query.after.map(|id| id.to_string()),
                    session.to_string(),
                    limit,
                    report_capability,
                ],
                StagingRow::read,
            )?
            .map(|entry| {
                let row = entry?;
                let media =
                    self.registered_file(&row.location, row.path.clone(), row.media_digest.clone());
                let annotation = match (
                    &row.annotation_path,
                    row.annotation_format.as_deref(),
                    &row.annotation_digest,
                ) {
                    (None, None, None) => Ok(None),
                    (Some(path), Some("COCO_IMAGE_BBOX"), digest) => self
                        .registered_file(&row.location, path.clone(), digest.clone())
                        .map(Some),
                    _ => Err(DatasetQueryError::InvalidImageBinding),
                };
                Ok(StagingCandidate {
                    row,
                    media,
                    annotation,
                })
            })
            .collect()
    }
}

impl StagingCandidate {
    fn state(&self) -> (StagingState, &'static str) {
        let row = &self.row;
        if row.availability != "ONLINE" {
            return (StagingState::Offline, "MEDIA_OFFLINE");
        }
        if row.kind != "IMAGE" {
            return (StagingState::Conflict, "EXPECTED_IMAGE");
        }
        let media = self
            .media
            .as_ref()
            .map(|file| file.live_digest(IMAGE_LIMIT));
        let annotation = self.annotation.as_ref().map(|file| {
            file.as_ref()
                .map(|value| value.live_digest(ANNOTATION_LIMIT))
                .transpose()
        });
        if matches!(media, Ok(Err(_))) || matches!(annotation, Ok(Err(_))) {
            return (StagingState::Offline, "REGISTERED_FILE_UNREADABLE");
        }
        if !matches!(media, Ok(Ok(true))) || !matches!(annotation, Ok(Ok(None | Some(true)))) {
            return (StagingState::Conflict, "REGISTERED_CONTENT_CHANGED");
        }
        if row.active_review.as_deref() == Some("CONFLICT") {
            return (StagingState::Conflict, "REVIEW_CONFLICT");
        }
        if row.checking {
            return (StagingState::Checking, "CHECK_RUNNING");
        }
        if row.annotation_path.is_none() {
            return (StagingState::NeedsAnnotation, "ANNOTATION_MISSING");
        }
        let schema = row
            .categories
            .as_ref()
            .map(|json| format!("{:x}", Sha256::digest(json.as_bytes())));
        if row.active_review.is_some()
            || schema.is_none()
            || row.final_review.as_deref() != Some("CONFIRMED")
            || row.media_digest != row.confirmed_media
            || row.annotation_digest != row.confirmed_annotation
            || schema != row.confirmed_schema
        {
            return (StagingState::NeedsReview, "CURRENT_FINAL_REQUIRED");
        }
        (StagingState::NeedsCheck, "SCOPED_REPORT_REQUIRED")
    }

    fn view(self, _root: &Path) -> StagingItem {
        let (state, reason) = self.state();
        let reason = Some(reason);
        #[cfg(feature = "execution-rpc")]
        let (state, reason, current_report_ref) = self.resolve_report(_root, state, reason);
        #[cfg(not(feature = "execution-rpc"))]
        let current_report_ref = None;
        let row = self.row;
        let source_frame = row
            .source_video_id
            .and_then(|video| parse_source(video, &row.source_metadata));
        StagingItem {
            asset_id: row.asset,
            target_dataset_id: row.dataset,
            display_name: row.name,
            state,
            review_state: row.active_review.or(row.final_review),
            source_frame,
            current_report_ref,
            blocking_reasons: reason.into_iter().collect(),
        }
    }

    #[cfg(feature = "execution-rpc")]
    fn resolve_report(
        &self,
        root: &Path,
        state: StagingState,
        reason: Option<&'static str>,
    ) -> (
        StagingState,
        Option<&'static str>,
        Option<ArtifactReference>,
    ) {
        if state != StagingState::NeedsCheck {
            return (state, reason, None);
        }
        let Some(report) = self
            .row
            .report
            .as_ref()
            .and_then(|stored| stored.load(root).ok())
        else {
            return (state, reason, None);
        };
        let row = &self.row;
        let scope = &report.data.scope;
        if Some(scope.dataset_id) != row.dataset
            || scope.asset_id != row.asset
            || Some(&scope.media_digest) != row.confirmed_media.as_ref()
            || scope.annotation_digest != row.confirmed_annotation
            || Some(&scope.label_schema_digest) != row.confirmed_schema.as_ref()
        {
            return (state, reason, None);
        }
        if report.data.required_geometry_passed() {
            return (
                StagingState::ReadyForAcceptance,
                None,
                Some(report.reference),
            );
        }
        (
            state,
            Some("REQUIRED_GEOMETRY_NOT_PASSED"),
            Some(report.reference),
        )
    }
}

#[derive(Deserialize)]
struct FrameSourceRecord {
    kind: String,
    frame: FrameIdentity,
    extraction_task_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct FrameIdentity {
    source_video_id: Uuid,
    source_digest: String,
    stream_index: u32,
    frame_index: String,
    pts_us: Option<String>,
}

fn parse_source(video: Uuid, json: &str) -> Option<StagingFrameSource> {
    let parsed: FrameSourceRecord = serde_json::from_str(json).ok()?;
    let frame = parsed.frame;
    if parsed.kind != "video_frame"
        || frame.source_video_id != video
        || parse_digest(&frame.source_digest).is_none()
    {
        return None;
    }
    let frame_index = frame.frame_index.parse::<u64>().ok()?;
    if frame_index.to_string() != frame.frame_index {
        return None;
    }
    let pts_us = match frame.pts_us {
        Some(text) => {
            let value = text.parse::<i64>().ok()?;
            if value < 0 || value.to_string() != text {
                return None;
            }
            Some(value)
        }
        None => None,
    };
    Some(StagingFrameSource {
        source_video_id: video,
        source_digest: frame.source_digest,
        stream_index: frame.stream_index,
        frame_index,
        pts_us,
        extraction_task_id: parsed.extraction_task_id,
    })
}

fn optional_uuid(row: &Row<'_>, column: usize) -> rusqlite::Result<Option<Uuid>> {
    let value: Option<String> = row.get(column)?;
    value
        .map(|text| Uuid::parse_str(&text).map_err(|_| rusqlite::Error::InvalidQuery))
        .transpose()
}
