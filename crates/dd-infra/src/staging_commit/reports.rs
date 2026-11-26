//! Reopen the exact checked reports named by a Staging confirmation.

use uuid::Uuid;

use super::{StagingCommitError, StagingCommitRequest};
use crate::artifact_reference::ArtifactOwner;
use crate::staging_report::{BATCH_KIND, StagingReport, read_batch_report, read_report};
use crate::workspace_thread::WorkspaceThread;

pub(super) fn accepted_reports(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
    request: &StagingCommitRequest,
) -> Result<Vec<StagingReport>, StagingCommitError> {
    let reports = if request.quality_report_ref.kind == BATCH_KIND {
        let batch = read_batch_report(owner, session, task).map_err(StagingCommitError::Report)?;
        if batch.reference != request.quality_report_ref
            || batch.asset_ids != request.asset_ids
            || batch.data.dataset_id != request.target_dataset_id
        {
            return Err(StagingCommitError::InvalidRequest);
        }
        batch
            .data
            .members
            .into_iter()
            .map(|member| {
                let ArtifactOwner::Task(source_task) = member.source_report_ref.owner else {
                    return Err(StagingCommitError::InvalidRequest);
                };
                let source =
                    read_report(owner, session, source_task).map_err(StagingCommitError::Report)?;
                if source.reference != member.source_report_ref || source.data.scope != member.scope
                {
                    return Err(StagingCommitError::InvalidRequest);
                }
                Ok(source)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        if request.asset_ids.len() != 1 {
            return Err(StagingCommitError::InvalidRequest);
        }
        let report = read_report(owner, session, task).map_err(StagingCommitError::Report)?;
        if report.reference != request.quality_report_ref {
            return Err(StagingCommitError::InvalidRequest);
        }
        vec![report]
    };
    for (report, expected_asset) in reports.iter().zip(&request.asset_ids) {
        let scope = &report.data.scope;
        if scope.dataset_id != request.target_dataset_id
            || scope.asset_id != *expected_asset
            || scope.label_schema_digest != request.expected_schema_fingerprint
        {
            return Err(StagingCommitError::InvalidRequest);
        }
        if scope.annotation_digest.is_none() || !report.data.required_geometry_passed() {
            return Err(StagingCommitError::NotReady);
        }
    }
    Ok(reports)
}
