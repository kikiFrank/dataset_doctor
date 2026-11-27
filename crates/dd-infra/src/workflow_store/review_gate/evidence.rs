use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{WorkflowError, WorkflowReviewEvidence, database, verify_error};
use crate::artifact_reference::ArtifactReference;
use crate::dataset_query::RegisteredImage;
use crate::geometry_runtime::read_bound_geometry_report;
use crate::sqlite::row_uuid;
use crate::staging_report::build_geometry_check;
use crate::workspace_thread::WorkspaceThread;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalReceipt {
    operation_id: Uuid,
    review_id: Uuid,
    annotation_digest: String,
    confirmed_negative: bool,
}

#[derive(Serialize)]
pub(in crate::workflow_store) struct VerifiedReview {
    asset_id: Uuid,
    final_operation_id: Uuid,
    final_review_id: Uuid,
    pub(super) annotation_digest: String,
    confirmed_negative: bool,
    geometry_task_id: Uuid,
    geometry_ref: ArtifactReference,
    geometry_input_fingerprint: String,
}

pub(in crate::workflow_store) fn verify(
    owner: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    original_report: Uuid,
    entry: &WorkflowReviewEvidence,
    now_ms: i64,
) -> Result<VerifiedReview, WorkflowError> {
    let (image, review, receipt, completed_at_ms) =
        verify_final(owner, session, dataset, original_report, entry)?;
    let geometry = read_bound_geometry_report(owner, session, entry.geometry_task_id, &image)
        .map_err(verify_error)?;
    if geometry.started_at_ms < completed_at_ms || geometry.finished_at_ms > now_ms {
        return Err(WorkflowError::InputChanged);
    }
    let geometry_ref = geometry.reference.clone();
    let geometry_input_fingerprint = geometry.input_fingerprint.clone();
    let assessment = build_geometry_check(entry.asset_id, geometry).map_err(verify_error)?;
    if assessment.execution_status != "COMPLETED"
        || assessment.processed_assets != 1
        || assessment
            .findings
            .iter()
            .any(|finding| finding.severity != "INFO")
    {
        return Err(WorkflowError::UpstreamNotReady);
    }
    Ok(VerifiedReview {
        asset_id: entry.asset_id,
        final_operation_id: entry.final_operation_id,
        final_review_id: review,
        annotation_digest: receipt.annotation_digest,
        confirmed_negative: receipt.confirmed_negative,
        geometry_task_id: entry.geometry_task_id,
        geometry_ref,
        geometry_input_fingerprint,
    })
}

pub(in crate::workflow_store) fn current_final(
    owner: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    original_report: Uuid,
    asset: Uuid,
) -> Result<(WorkflowReviewEvidence, String), WorkflowError> {
    let operation = owner
        .workspace_action(session, move |owner| {
            owner
                .connection
                .query_row(
                    "SELECT f.operation_id FROM dataset_assets m
             JOIN file_operations f ON f.review_id=m.final_review_id
               AND f.dataset_id=m.dataset_id AND f.asset_id=m.asset_id
             WHERE m.dataset_id=?1 AND m.asset_id=?2 AND f.status='COMMITTED'
               AND f.operation_kind='REVIEW_COMMIT'
             ORDER BY f.completed_at_ms DESC LIMIT 1",
                    params![dataset.to_string(), asset.to_string()],
                    |row| row_uuid(row, 0),
                )
                .optional()
                .map_err(database)
        })
        .map_err(WorkflowError::Storage)?
        .ok_or(WorkflowError::UpstreamNotReady)?;
    let evidence = WorkflowReviewEvidence {
        asset_id: asset,
        final_operation_id: operation,
        geometry_task_id: Uuid::nil(),
    };
    let (_, _, receipt, _) = verify_final(owner, session, dataset, original_report, &evidence)?;
    Ok((evidence, receipt.annotation_digest))
}

fn verify_final(
    owner: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    original_report: Uuid,
    entry: &WorkflowReviewEvidence,
) -> Result<(RegisteredImage, Uuid, FinalReceipt, i64), WorkflowError> {
    let asset = entry.asset_id;
    let operation = entry.final_operation_id;
    let image = owner
        .registered_image(session, dataset, asset)
        .map_err(WorkflowError::Storage)?;
    let annotation = image
        .annotation
        .as_ref()
        .ok_or(WorkflowError::InputChanged)?;
    let current = owner.workspace_action(session, move |owner| {
        owner.connection.query_row(
            "SELECT r.review_id, r.confirmed_media_digest, r.confirmed_label_schema_digest,
                    r.confirmed_annotation_digest, f.result_json, f.completed_at_ms
             FROM dataset_assets AS m
               JOIN review_sessions AS r ON m.final_review_id=r.review_id AND r.asset_id=m.asset_id AND r.dataset_id=m.dataset_id
               JOIN file_operations AS f ON f.review_id=r.review_id AND f.asset_id=m.asset_id AND f.dataset_id=m.dataset_id
               JOIN tasks AS t ON t.task_id=f.task_id AND t.dataset_id=m.dataset_id
             WHERE m.dataset_id=?1 AND m.asset_id=?2 AND f.operation_id=?3
               AND r.status='CONFIRMED' AND f.operation_kind='REVIEW_COMMIT' AND f.status='COMMITTED'
               AND t.operation_kind='REVIEW_COMMIT' AND t.status='SUCCEEDED'
               AND f.completed_at_ms >= (SELECT finished_at_ms FROM tasks WHERE task_id=?4)",
            params![dataset.to_string(), asset.to_string(), operation.to_string(), original_report.to_string()],
            |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, i64>(5)?)),
        ).optional().map_err(database)
    }).map_err(WorkflowError::Storage)?.ok_or(WorkflowError::UpstreamNotReady)?;
    let (review, media, schema, confirmed_annotation, raw_receipt, completed_at_ms) = current;
    let receipt: FinalReceipt = serde_json::from_str(&raw_receipt).map_err(WorkflowError::Json)?;
    if receipt.operation_id != operation
        || receipt.review_id != review
        || receipt.annotation_digest != annotation.sha256
        || confirmed_annotation != annotation.sha256
        || media != image.image.sha256
        || schema != image.label_schema_fingerprint
    {
        return Err(WorkflowError::InputChanged);
    }
    Ok((image, review, receipt, completed_at_ms))
}
