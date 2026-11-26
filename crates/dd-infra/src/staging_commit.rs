//! Explicitly admit a checked and confirmed staged image into its target Dataset.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::input_fingerprint::{InputFingerprintError, freeze_input_manifest};
use crate::json::JsonContracts;
use crate::paths::publish_immutable;
use crate::sqlite::row_uuid;
use crate::staging_report::{
    ReportScope, StagingReportError, image_scope, read_report, staged_image, verify_image,
};
use crate::task_store::{TaskStoreError, TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace::OpenedWorkspace;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const KIND: &str = "STAGING_COMMIT";
const CAPABILITY: &str = "staging_commit.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcknowledgedFinding {
    pub issue_id: Uuid,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcknowledgedIncompleteCheck {
    pub check_id: String,
    pub reason: String,
}

#[derive(Clone)]
pub struct StagingCommitRequest {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub target_dataset_id: Uuid,
    pub asset_ids: Vec<Uuid>,
    pub quality_report_ref: ArtifactReference,
    pub acknowledged_findings: Vec<AcknowledgedFinding>,
    pub acknowledged_incomplete_checks: Vec<AcknowledgedIncompleteCheck>,
    pub expected_schema_fingerprint: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StagingCommitReceipt {
    pub task_id: Uuid,
    pub target_dataset_id: Uuid,
    pub accepted_asset_ids: Vec<Uuid>,
    pub quality_report_ref: ArtifactReference,
    pub acknowledged_findings: Vec<AcknowledgedFinding>,
    pub acknowledged_incomplete_checks: Vec<AcknowledgedIncompleteCheck>,
    pub accepted_at_ms: i64,
}

#[derive(Debug)]
pub enum StagingCommitError {
    Workspace(WorkspaceThreadError),
    Report(StagingReportError),
    Input(InputFingerprintError),
    Io(io::Error),
    Setup(Box<dyn Error + Send + Sync>),
    InvalidRequest,
    AcknowledgementRequired,
    NotReady,
    InvalidReceipt,
}

impl Display for StagingCommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(error) => Display::fmt(error, formatter),
            Self::Report(error) => Display::fmt(error, formatter),
            Self::Input(error) => Display::fmt(error, formatter),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Setup(error) => Display::fmt(error, formatter),
            Self::InvalidRequest => {
                formatter.write_str("The request does not match the checked Staging scope")
            }
            Self::AcknowledgementRequired => {
                formatter.write_str("Explicit acknowledgements must match the report")
            }
            Self::NotReady => formatter
                .write_str("Staging no longer has the required current report and confirmed Final"),
            Self::InvalidReceipt => {
                formatter.write_str("The persisted Staging acceptance receipt is invalid")
            }
        }
    }
}

impl Error for StagingCommitError {}

impl From<WorkspaceThreadError> for StagingCommitError {
    fn from(error: WorkspaceThreadError) -> Self {
        Self::Workspace(error)
    }
}

pub struct StagingCommitService {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

impl StagingCommitService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, StagingCommitError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(StagingCommitError::Setup)?,
        })
    }

    pub fn commit(
        &self,
        session: Uuid,
        mut request: StagingCommitRequest,
        now: i64,
    ) -> Result<StagingCommitReceipt, StagingCommitError> {
        let [asset] = request.asset_ids.as_slice() else {
            return Err(StagingCommitError::InvalidRequest);
        };
        let asset = *asset;
        let ArtifactOwner::Task(report_task) = request.quality_report_ref.owner else {
            return Err(StagingCommitError::InvalidRequest);
        };
        if request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || now < 0
            || asset.is_nil()
            || request.target_dataset_id.is_nil()
            || report_task.is_nil()
        {
            return Err(StagingCommitError::InvalidRequest);
        }
        request
            .acknowledged_findings
            .sort_unstable_by_key(|ack| ack.issue_id);
        request
            .acknowledged_incomplete_checks
            .sort_unstable_by(|left, right| left.check_id.cmp(&right.check_id));
        let dataset = request.target_dataset_id;
        let intent = json!({
            "target_dataset_id":dataset, "asset_ids":request.asset_ids,
            "quality_report_ref":request.quality_report_ref,
            "acknowledged_findings":request.acknowledged_findings,
            "acknowledged_incomplete_checks":request.acknowledged_incomplete_checks,
            "expected_schema_fingerprint":request.expected_schema_fingerprint,
        });
        let fingerprint: [u8; 32] = Sha256::digest(intent.to_string().as_bytes()).into();
        let _guard = self.owner.lock_final_operations()?;
        let key = request.idempotency_key.to_string();
        let replay = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    "SELECT task_id,request_fingerprint,status,
                        json_extract(execution_metadata_json,'$.staging_commit_receipt')
                 FROM tasks WHERE operation_kind=?1 AND capability_id=?2 AND idempotency_key=?3",
                    params![KIND, CAPABILITY, key],
                    |row| {
                        Ok((
                            row_uuid(row, 0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(database)
        })?;
        if let Some((task, original, status, encoded_receipt)) = replay {
            if original != format_digest(&fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            let receipt: StagingCommitReceipt = encoded_receipt
                .as_deref()
                .and_then(|encoded| serde_json::from_str(encoded).ok())
                .ok_or(StagingCommitError::InvalidReceipt)?;
            if status != "SUCCEEDED"
                || receipt.task_id != task
                || receipt.target_dataset_id != dataset
                || receipt.accepted_asset_ids != [asset]
                || receipt.quality_report_ref != request.quality_report_ref
                || receipt.acknowledged_findings != request.acknowledged_findings
                || receipt.acknowledged_incomplete_checks != request.acknowledged_incomplete_checks
            {
                return Err(StagingCommitError::InvalidReceipt);
            }
            return Ok(receipt);
        }

        let report =
            read_report(&self.owner, session, report_task).map_err(StagingCommitError::Report)?;
        let scope = &report.data.scope;
        if report.reference != request.quality_report_ref
            || scope.dataset_id != dataset
            || scope.asset_id != asset
            || scope.label_schema_digest != request.expected_schema_fingerprint
        {
            return Err(StagingCommitError::InvalidRequest);
        }
        if scope.annotation_digest.is_none() || !report.data.required_geometry_passed() {
            return Err(StagingCommitError::NotReady);
        }
        let mut incomplete = report.data.unexecuted_checks.clone();
        incomplete.sort_unstable();
        let mut findings: Vec<Uuid> = report
            .data
            .checks
            .iter()
            .flat_map(|check| &check.findings)
            .filter(|finding| finding.severity != "INFO")
            .map(|finding| finding.issue_id)
            .collect();
        findings.sort_unstable();
        if !incomplete.iter().map(String::as_str).eq(request
            .acknowledged_incomplete_checks
            .iter()
            .map(|ack| ack.check_id.as_str()))
            || request
                .acknowledged_incomplete_checks
                .iter()
                .any(|ack| ack.reason.trim().is_empty())
            || !findings
                .iter()
                .copied()
                .eq(request.acknowledged_findings.iter().map(|ack| ack.issue_id))
        {
            return Err(StagingCommitError::AcknowledgementRequired);
        }
        let image = self.owner.workspace_action(session, move |workspace| {
            staged_image(workspace, dataset, asset)
        })?;
        if image_scope(&image) != *scope || !verify_image(&image).map_err(StagingCommitError::Io)? {
            return Err(StagingCommitError::NotReady);
        }
        let input = freeze_input_manifest(
            &self.contracts,
            json!({
                "capability_id":CAPABILITY,"implementation_version":"1","model_identity":null,
                "dataset_id":dataset,"label_schema_digest":scope.label_schema_digest,
                "assets":[{"asset_id":asset,"media_digest":scope.media_digest,
                    "annotation_digest":scope.annotation_digest,"source_metadata_digest":null}],
                "resolved_config":intent,
            }),
        )
        .map_err(StagingCommitError::Input)?;
        let path = PathBuf::from(format!(
            "artifacts/task-inputs/{}.json",
            format_digest(&input.sha256)
        ));
        let root = self.owner.storage_root(session)?;
        publish_immutable(&root, &path, &input.bytes).map_err(StagingCommitError::Io)?;
        let submission = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(dataset),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: KIND.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: path,
            request_fingerprint: fingerprint,
            input_fingerprint: input.input_fingerprint,
        };
        self.owner.workspace_action(session, move |workspace| {
            let current = staged_image(workspace, dataset, asset)?;
            if image_scope(&current) != report.data.scope {
                return Ok(Err(StagingCommitError::NotReady));
            }
            Ok(commit_membership(
                workspace,
                &submission,
                report.data.scope,
                report_task,
                request,
                now,
            ))
        })?
    }
}

fn commit_membership(
    workspace: &mut OpenedWorkspace,
    submission: &TaskSubmission,
    scope: ReportScope,
    report_task: Uuid,
    request: StagingCommitRequest,
    now: i64,
) -> Result<StagingCommitReceipt, StagingCommitError> {
    let tx = workspace
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database)?;
    let asset = scope.asset_id.to_string();
    let dataset = scope.dataset_id.to_string();
    let review = tx.query_row(
        "SELECT r.review_id
         FROM staging_items s JOIN review_sessions r
            ON r.review_id=s.review_id AND r.asset_id=s.asset_id AND r.dataset_id=s.target_dataset_id
         WHERE s.asset_id=?1 AND s.target_dataset_id=?2 AND s.check_task_id=?3
           AND r.status='CONFIRMED' AND r.confirmed_media_digest=?4
           AND r.confirmed_annotation_digest=?5 AND r.confirmed_label_schema_digest=?6
           AND NOT EXISTS (SELECT 1 FROM review_sessions active
               WHERE active.asset_id=s.asset_id AND active.status IN ('OPEN','COMMITTING','CONFLICT'))",
        params![asset,dataset,report_task.to_string(),scope.media_digest,scope.annotation_digest,scope.label_schema_digest],
        |row| row.get::<_,String>(0),
    ).optional().map_err(database)?.ok_or(StagingCommitError::NotReady)?;
    let task = enqueue_task_in(&tx, submission, now)
        .map_err(WorkspaceThreadError::Task)?
        .task_id;
    tx.execute(
        "INSERT INTO dataset_assets(dataset_id,asset_id,final_review_id,added_at_ms) VALUES (?1,?2,?3,?4)",
        params![dataset,asset,review,now],
    ).map_err(database)?;
    let removed = tx.execute(
        "DELETE FROM staging_items WHERE asset_id=?1 AND target_dataset_id=?2 AND check_task_id=?3",
        params![asset,dataset,report_task.to_string()],
    ).map_err(database)?;
    let revised = tx
        .execute(
            "UPDATE datasets SET row_revision=row_revision+1,updated_at_ms=max(updated_at_ms,?2)
         WHERE dataset_id=?1",
            params![dataset, now],
        )
        .map_err(database)?;
    if removed != 1 || revised != 1 {
        return Err(StagingCommitError::NotReady);
    }
    let receipt = StagingCommitReceipt {
        task_id: task,
        target_dataset_id: scope.dataset_id,
        accepted_asset_ids: vec![scope.asset_id],
        quality_report_ref: request.quality_report_ref,
        acknowledged_findings: request.acknowledged_findings,
        acknowledged_incomplete_checks: request.acknowledged_incomplete_checks,
        accepted_at_ms: now,
    };
    let metadata = json!({"staging_commit_receipt":receipt}).to_string();
    let changed = tx.execute(
        "UPDATE tasks SET status='SUCCEEDED',started_at_ms=?2,finished_at_ms=?2,updated_at_ms=?2,
            row_revision=row_revision+1,execution_metadata_json=?3 WHERE task_id=?1 AND status='QUEUED'",
        params![task.to_string(),now,metadata],
    ).map_err(database)?;
    if changed != 1 {
        return Err(StagingCommitError::InvalidReceipt);
    }
    tx.commit().map_err(database)?;
    Ok(receipt)
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
