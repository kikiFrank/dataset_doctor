//! Produce and reopen deterministic quality reports for Staging.
mod batch;

pub(crate) use batch::{BATCH_KIND, MAX_BATCH_ASSETS, manifest_asset, read_batch_report};
pub use batch::{StagingBatchReport, StagingBatchReportRequest};

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::dataset_query::RegisteredImage;
use crate::geometry_runtime::{BoundGeometryReport, read_bound_geometry_report};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, strict_json};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, enqueue_task_in, format_digest};
use crate::worker_runtime::now_ms;
use crate::workspace::OpenedWorkspace;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub(crate) const KIND: &str = "BUILD_REPORT";
pub(crate) const CAPABILITY: &str = "staging_report.v1";
const ARTIFACT_KIND: &str = "STAGING_CHECK_REPORT";
const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;
const GEOMETRY: &str = "annotation_geometry";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionalStagingCheck {
    SemanticReview,
}

#[derive(Clone)]
pub struct StagingReportRequest {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub target_dataset_id: Uuid,
    pub asset_id: Uuid,
    pub geometry_task_id: Uuid,
    pub optional_checks: Vec<OptionalStagingCheck>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReportScope {
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub media_digest: String,
    pub annotation_digest: Option<String>,
    pub label_schema_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReportFinding {
    pub issue_id: Uuid,
    pub asset_id: Uuid,
    pub check_id: String,
    pub finding_code: String,
    pub severity: String,
    pub input_fingerprint: String,
    pub explanation: String,
    pub annotation_id: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CheckReason {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReportCheck {
    pub check_id: String,
    pub check_contract_version: String,
    pub execution_status: String,
    pub assessment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_or_failure_reason: Option<CheckReason>,
    pub input_fingerprint: String,
    pub processed_assets: u32,
    pub total_assets: u32,
    pub finding_count: usize,
    pub findings: Vec<ReportFinding>,
    pub evidence_refs: Vec<ArtifactReference>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StagingReportData {
    pub contract_version: u32,
    pub report_id: Uuid,
    pub scope: ReportScope,
    pub input_fingerprint: String,
    pub execution_coverage: BTreeMap<String, u32>,
    pub findings_by_severity: BTreeMap<String, u32>,
    pub checks: Vec<ReportCheck>,
    pub unexecuted_checks: Vec<String>,
    pub repair_outcomes: Vec<serde_json::Value>,
    pub human_ack_status: String,
}

#[derive(Debug, Serialize)]
pub struct StagingReport {
    pub task_id: Uuid,
    pub reference: ArtifactReference,
    pub data: StagingReportData,
}

#[derive(Debug)]
pub enum StagingReportError {
    Workspace(WorkspaceThreadError),
    Io(io::Error),
    Evidence(Box<dyn Error + Send + Sync>),
    InvalidInput,
    InvalidReport,
}

impl Display for StagingReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(error) => Display::fmt(error, formatter),
            Self::Io(error) => write!(formatter, "Access Staging report: {error}"),
            Self::Evidence(error) => Display::fmt(error, formatter),
            Self::InvalidInput => formatter.write_str("Staging report input is missing or changed"),
            Self::InvalidReport => formatter.write_str("Stored Staging report failed validation"),
        }
    }
}

impl Error for StagingReportError {}

impl From<WorkspaceThreadError> for StagingReportError {
    fn from(error: WorkspaceThreadError) -> Self {
        Self::Workspace(error)
    }
}

impl From<io::Error> for StagingReportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct StagingReportService {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

impl StagingReportService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, StagingReportError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(StagingReportError::Evidence)?,
        })
    }

    /// Publish a report when a desktop geometry task belongs to Staging.
    /// Ordinary Dataset members continue to expose the geometry result itself.
    pub fn build_after_geometry(
        owner: Arc<WorkspaceThread>,
        session: Uuid,
        request: StagingReportRequest,
    ) -> Result<Option<StagingReport>, StagingReportError> {
        let dataset = request.target_dataset_id;
        let asset = request.asset_id;
        let staged = owner.workspace_action(session, move |workspace| {
            workspace.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM staging_items WHERE asset_id=?1 AND target_dataset_id=?2)",
                params![asset.to_string(), dataset.to_string()], |row| row.get::<_, bool>(0),
            ).map_err(database)
        })?;
        if !staged {
            return Ok(None);
        }
        let timestamp = now_ms().map_err(|error| StagingReportError::Evidence(Box::new(error)))?;
        Self::new(owner)?
            .build(session, request, timestamp)
            .map(Some)
    }

    pub fn build(
        &self,
        session: Uuid,
        mut request: StagingReportRequest,
        now: i64,
    ) -> Result<StagingReport, StagingReportError> {
        if request.request_id.is_nil() || request.idempotency_key.is_nil() || now < 0 {
            return Err(StagingReportError::InvalidInput);
        }
        request.optional_checks.sort_unstable();
        if request
            .optional_checks
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(StagingReportError::InvalidInput);
        }
        let _guard = self.owner.lock_final_operations()?;
        let dataset = request.target_dataset_id;
        let asset = request.asset_id;
        let options = json!({
            "geometry_task_id":request.geometry_task_id,
            "optional_checks":request.optional_checks,
        });
        let intent = json!({"dataset_id":dataset, "asset_id":asset, "options":options});
        let fingerprint: [u8; 32] = Sha256::digest(intent.to_string().as_bytes()).into();
        let key = request.idempotency_key.to_string();
        let existing = self.owner.workspace_action(session, move |workspace| {
            workspace.connection.query_row(
                "SELECT task_id,request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
                params![KIND,key], |row| Ok((row_uuid(row,0)?,row.get::<_,String>(1)?)),
            ).optional().map_err(database)
        })?;
        if let Some((task, original)) = existing {
            if original != format_digest(&fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return self.get(session, task);
        }

        let image = self.owner.workspace_action(session, move |workspace| {
            staged_image(workspace, dataset, asset)
        })?;
        if !verify_image(&image)? {
            return Err(StagingReportError::InvalidInput);
        }
        let evidence =
            read_bound_geometry_report(&self.owner, session, request.geometry_task_id, &image)
                .map_err(|error| StagingReportError::Evidence(Box::new(error)))?;
        let scope = image_scope(&image);
        let input = freeze_input_manifest(
            &self.contracts,
            json!({
                "capability_id":CAPABILITY,"implementation_version":"1","model_identity":null,
                "dataset_id":dataset,"label_schema_digest":scope.label_schema_digest,
                "assets":[{"asset_id":asset,"media_digest":scope.media_digest,
                    "annotation_digest":scope.annotation_digest,"source_metadata_digest":null}],
                "resolved_config":{"options":options,"geometry_evidence":evidence.reference},
            }),
        )
        .map_err(|error| StagingReportError::Evidence(Box::new(error)))?;
        let root = self.owner.storage_root(session)?;
        let path = PathBuf::from(format!(
            "artifacts/task-inputs/{}.json",
            format_digest(&input.sha256)
        ));
        publish_immutable(&root, &path, &input.bytes)?;
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
        let checks = build_checks(
            asset,
            evidence,
            &request.optional_checks,
            &format_digest(&input.input_fingerprint),
        )?;
        self.owner.workspace_action(session, move |workspace| {
            let current = image_scope(&staged_image(workspace, dataset, asset)?);
            if current != scope {
                return Ok(Err(StagingReportError::InvalidInput));
            }
            Ok(publish_report(workspace, &submission, scope, checks, now))
        })?
    }

    /// Return historical accepted evidence after reverifying its stored bytes.
    pub fn get(&self, session: Uuid, task: Uuid) -> Result<StagingReport, StagingReportError> {
        read_report(&self.owner, session, task)
    }
}

pub(crate) fn read_report(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
) -> Result<StagingReport, StagingReportError> {
    let (root, stored) = owner.workspace_action(session, move |workspace| {
                workspace.connection.query_row(
                    "SELECT task_id,output_manifest_path,input_fingerprint,dataset_id,
                        json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
                     FROM tasks WHERE task_id=?1 AND operation_kind=?2 AND capability_id=?3 AND status='SUCCEEDED'",
                    params![task.to_string(),KIND,CAPABILITY],
                    |row| Ok((workspace.root().to_owned(), StoredReport::read(row, 0)?)),
                ).optional().map_err(database)?.ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
            })?;
    stored.load(&root)
}

pub(crate) struct StoredReport {
    task: Uuid,
    path: String,
    fingerprint: String,
    dataset: Uuid,
    encoded_ref: String,
}

impl StoredReport {
    pub(crate) fn read(row: &Row<'_>, start: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            task: row_uuid(row, start)?,
            path: row.get(start + 1)?,
            fingerprint: row.get(start + 2)?,
            dataset: row_uuid(row, start + 3)?,
            encoded_ref: row.get(start + 4)?,
        })
    }

    pub(crate) fn load(&self, root: &Path) -> Result<StagingReport, StagingReportError> {
        let invalid = || StagingReportError::InvalidReport;
        let reference: ArtifactReference =
            serde_json::from_str(&self.encoded_ref).map_err(|_| invalid())?;
        if self.path != report_path(self.task)
            || reference.owner != ArtifactOwner::Task(self.task)
            || reference.kind != ARTIFACT_KIND
            || reference.media_type != "application/json"
            || reference.artifact_id.is_nil()
        {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        open_scoped_file(root, Path::new(&self.path))?
            .take(MAX_REPORT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_REPORT_BYTES
            || bytes.len().to_string() != reference.size_bytes
            || format!("{:x}", Sha256::digest(&bytes)) != reference.sha256
        {
            return Err(invalid());
        }
        let data: StagingReportData =
            serde_json::from_value(strict_json(&bytes).map_err(|_| invalid())?)
                .map_err(|_| invalid())?;
        if data.contract_version != 1
            || data.report_id != self.task
            || data.input_fingerprint != self.fingerprint
            || data.scope.dataset_id != self.dataset
        {
            return Err(invalid());
        }
        Ok(StagingReport {
            task_id: self.task,
            reference,
            data,
        })
    }
}

impl StagingReportData {
    pub(crate) fn required_geometry_passed(&self) -> bool {
        self.checks.iter().any(|check| {
            check.check_id == GEOMETRY
                && check.execution_status == "COMPLETED"
                && check.assessment == "PASS"
                && (check.processed_assets, check.total_assets) == (1, 1)
        })
    }
}

pub(crate) fn staged_image(
    workspace: &OpenedWorkspace,
    dataset: Uuid,
    asset: Uuid,
) -> Result<RegisteredImage, WorkspaceThreadError> {
    workspace
        .connection
        .query_row(
            "SELECT 1 FROM staging_items WHERE asset_id=?1 AND target_dataset_id=?2",
            params![asset.to_string(), dataset.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map_err(database)?
        .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))?;
    workspace
        .registered_image(dataset, asset)
        .map_err(WorkspaceThreadError::Dataset)
}

pub(crate) fn image_scope(image: &RegisteredImage) -> ReportScope {
    ReportScope {
        dataset_id: image.dataset_id,
        asset_id: image.asset_id,
        media_digest: image.image.sha256.clone(),
        annotation_digest: image
            .annotation
            .as_ref()
            .map(|annotation| annotation.sha256.clone()),
        label_schema_digest: image.label_schema_fingerprint.clone(),
    }
}

pub(crate) fn verify_image(image: &RegisteredImage) -> io::Result<bool> {
    Ok(image.image.live_digest(256 * 1024 * 1024)?
        && image
            .annotation
            .as_ref()
            .map(|file| file.live_digest(16 * 1024 * 1024))
            .transpose()?
            != Some(false))
}

pub(crate) fn build_geometry_check(
    asset: Uuid,
    evidence: BoundGeometryReport,
) -> Result<ReportCheck, StagingReportError> {
    let completed = match evidence.report.status.as_str() {
        "COMPLETED" => true,
        "NOT_EXECUTED" => false,
        _ => return Err(StagingReportError::InvalidReport),
    };
    let findings: Vec<_> = evidence
        .report
        .findings
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let key = format!("{}:{GEOMETRY}:{index}", evidence.input_fingerprint);
            let digest = Sha256::digest(key.as_bytes());
            let (severity, explanation) = match entry.code.as_str() {
                "annotation_empty" => ("INFO", "The COCO document contains no bounding boxes."),
                "annotation_missing" => ("INFO", "The image has no annotation document."),
                "dimension_mismatch" => (
                    "ERROR",
                    "The declared image dimensions differ from the decoded image.",
                ),
                "duplicate_category_id" => (
                    "ERROR",
                    "The COCO category table contains a duplicate identifier.",
                ),
                "duplicate_annotation_id" => (
                    "ERROR",
                    "More than one annotation uses the same identifier.",
                ),
                "image_reference_mismatch" => (
                    "ERROR",
                    "The annotation references a different image identifier.",
                ),
                "unknown_category" => ("ERROR", "The annotation references an undefined category."),
                "bbox_out_of_bounds" => ("ERROR", "The bounding box extends outside the image."),
                "invalid_bbox" => (
                    "ERROR",
                    "The bounding box does not have finite coordinates and positive dimensions.",
                ),
                _ => ("ERROR", "The geometry check reported a structural problem."),
            };
            ReportFinding {
                issue_id: uuid::Builder::from_custom_bytes(
                    digest[..16].try_into().expect("16 digest bytes"),
                )
                .into_uuid(),
                asset_id: asset,
                check_id: GEOMETRY.into(),
                severity: severity.into(),
                finding_code: entry.code,
                explanation: explanation.into(),
                input_fingerprint: evidence.input_fingerprint.clone(),
                annotation_id: entry.annotation_id,
            }
        })
        .collect();
    let assessment = if !completed {
        "NOT_EVALUATED"
    } else if findings.iter().any(|entry| entry.severity == "ERROR") {
        "FINDINGS"
    } else {
        "PASS"
    };
    Ok(ReportCheck {
        check_id: GEOMETRY.into(),
        check_contract_version: "1".into(),
        execution_status: if completed { "COMPLETED" } else { "SKIPPED" }.into(),
        assessment: assessment.into(),
        skip_or_failure_reason: (!completed).then(|| CheckReason {
            code: "ANNOTATION_MISSING".into(),
            message: "No annotation document was available for geometry checks.".into(),
        }),
        input_fingerprint: evidence.input_fingerprint,
        finding_count: findings.len(),
        findings,
        evidence_refs: vec![evidence.reference],
        processed_assets: u32::from(completed),
        total_assets: 1,
        started_at: Some(timestamp(evidence.started_at_ms)?),
        finished_at: Some(timestamp(evidence.finished_at_ms)?),
    })
}

fn build_checks(
    asset: Uuid,
    evidence: BoundGeometryReport,
    optional: &[OptionalStagingCheck],
    aggregate_fingerprint: &str,
) -> Result<Vec<ReportCheck>, StagingReportError> {
    let mut checks = vec![build_geometry_check(asset, evidence)?];
    checks.extend(optional.iter().map(|check| {
        ReportCheck {
            check_id: match check {
                OptionalStagingCheck::SemanticReview => "semantic_review",
            }
            .into(),
            check_contract_version: "1".into(),
            execution_status: "SKIPPED".into(),
            assessment: "NOT_EVALUATED".into(),
            skip_or_failure_reason: Some(CheckReason {
                code: "NO_ACCEPTED_EVIDENCE".into(),
                message: "No verified semantic review was supplied for this sample.".into(),
            }),
            input_fingerprint: aggregate_fingerprint.into(),
            processed_assets: 0,
            total_assets: 1,
            finding_count: 0,
            findings: vec![],
            evidence_refs: vec![],
            started_at: None,
            finished_at: None,
        }
    }));
    Ok(checks)
}

fn publish_report(
    workspace: &mut OpenedWorkspace,
    submission: &TaskSubmission,
    scope: ReportScope,
    checks: Vec<ReportCheck>,
    now: i64,
) -> Result<StagingReport, StagingReportError> {
    let root = workspace.root().to_owned();
    let tx = workspace.connection.transaction().map_err(database)?;
    let task = enqueue_task_in(&tx, submission, now)
        .map_err(WorkspaceThreadError::Task)?
        .task_id;
    let fingerprint = format_digest(&submission.input_fingerprint);
    let mut execution_coverage = BTreeMap::new();
    let mut findings_by_severity = BTreeMap::new();
    let mut unexecuted_checks = Vec::new();
    for check in &checks {
        *execution_coverage
            .entry(check.execution_status.clone())
            .or_insert(0) += 1;
        if check.assessment == "NOT_EVALUATED" {
            unexecuted_checks.push(check.check_id.clone());
        }
        for finding in &check.findings {
            *findings_by_severity
                .entry(finding.severity.clone())
                .or_insert(0) += 1;
        }
    }
    let data = StagingReportData {
        contract_version: 1,
        report_id: task,
        scope,
        input_fingerprint: fingerprint,
        execution_coverage,
        findings_by_severity,
        checks,
        unexecuted_checks,
        repair_outcomes: vec![],
        human_ack_status: "PENDING".into(),
    };
    let bytes = serde_json::to_vec(&data).map_err(|_| StagingReportError::InvalidReport)?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return Err(StagingReportError::InvalidReport);
    }
    let path = report_path(task);
    publish_immutable(&root, Path::new(&path), &bytes)?;
    let reference = ArtifactReference {
        artifact_id: Uuid::new_v4(),
        owner: ArtifactOwner::Task(task),
        kind: ARTIFACT_KIND.into(),
        media_type: "application/json".into(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        size_bytes: bytes.len().to_string(),
        schema_id: None,
    };
    let metadata = json!({"terminal_receipt":{"result_manifest_ref":reference}}).to_string();
    let changed = tx.execute(
        "UPDATE tasks SET status='SUCCEEDED',started_at_ms=?2,finished_at_ms=?2,updated_at_ms=?2,
             row_revision=row_revision+1,output_manifest_path=?3,execution_metadata_json=?4
         WHERE task_id=?1 AND status='QUEUED'",
        params![task.to_string(),now,path,metadata],
    ).map_err(database)?;
    let staged = tx.execute(
        "UPDATE staging_items SET check_task_id=?1,check_acceptance_digest=NULL,check_accepted_at_ms=NULL
         WHERE asset_id=?2 AND target_dataset_id=?3",
        params![task.to_string(),data.scope.asset_id.to_string(),data.scope.dataset_id.to_string()],
    ).map_err(database)?;
    if changed != 1 || staged != 1 {
        return Err(StagingReportError::InvalidInput);
    }
    tx.commit().map_err(database)?;
    Ok(StagingReport {
        task_id: task,
        reference,
        data,
    })
}

pub(crate) fn timestamp(milliseconds: i64) -> Result<String, StagingReportError> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(milliseconds) * 1_000_000)
        .ok()
        .and_then(|date| {
            date.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .ok_or(StagingReportError::InvalidReport)
}

fn report_path(task: Uuid) -> String {
    format!("artifacts/tasks/{task}/report.json")
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
