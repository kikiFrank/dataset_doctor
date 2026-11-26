//! Seal the evidence for one selection of checked Staging samples.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    KIND, MAX_REPORT_BYTES, ReportScope, StagingReportError, StagingReportService, database,
    image_scope, read_report, report_path, staged_image, verify_image,
};
use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::strict_json;
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub(crate) const BATCH_KIND: &str = "STAGING_BATCH_CHECK_REPORT";
pub(crate) const MAX_BATCH_ASSETS: usize = 1_000;
const CAPABILITY: &str = "staging_batch_report.v1";

#[derive(Clone)]
pub struct StagingBatchReportRequest {
    pub target_dataset_id: Uuid,
    pub source_report_refs: Vec<ArtifactReference>,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

#[derive(Debug)]
pub struct StagingBatchReport {
    pub task_id: Uuid,
    pub reference: ArtifactReference,
    pub asset_ids: Vec<Uuid>,
    pub(crate) data: BatchReportData,
}

impl StagingBatchReport {
    pub fn label_schema_digest(&self) -> &str {
        &self.data.label_schema_digest
    }

    pub fn unexecuted_checks(&self) -> &BTreeSet<String> {
        &self.data.unexecuted_checks
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchReportData {
    pub(crate) contract_version: u32,
    pub(crate) report_id: Uuid,
    pub(crate) dataset_id: Uuid,
    pub(crate) label_schema_digest: String,
    pub(crate) input_fingerprint: String,
    pub(crate) members: Vec<BatchMember>,
    pub(crate) execution_coverage: BTreeMap<String, u32>,
    pub(crate) findings_by_severity: BTreeMap<String, u32>,
    pub(crate) unexecuted_checks: BTreeSet<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchMember {
    pub(crate) scope: ReportScope,
    pub(crate) source_report_ref: ArtifactReference,
}

impl StagingReportService {
    pub fn build_batch(
        &self,
        session: Uuid,
        mut request: StagingBatchReportRequest,
        now: i64,
    ) -> Result<StagingBatchReport, StagingReportError> {
        if !(2..=MAX_BATCH_ASSETS).contains(&request.source_report_refs.len())
            || request.target_dataset_id.is_nil()
            || request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || now < 0
        {
            return Err(StagingReportError::InvalidInput);
        }
        request
            .source_report_refs
            .sort_unstable_by_key(|reference| reference.artifact_id);
        let intent = json!({
            "dataset_id": request.target_dataset_id, "source_report_refs": request.source_report_refs,
        });
        let fingerprint: [u8; 32] = Sha256::digest(intent.to_string().as_bytes()).into();
        let _guard = self.owner.lock_final_operations()?;
        let key = request.idempotency_key.to_string();
        let previous = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    "SELECT task_id,request_fingerprint FROM tasks
                 WHERE operation_kind=?1 AND capability_id=?2 AND idempotency_key=?3",
                    params![KIND, CAPABILITY, key],
                    |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(database)
        })?;
        if let Some((task, previous_fingerprint)) = previous {
            if previous_fingerprint != format_digest(&fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return read_batch_report(&self.owner, session, task);
        }
        let dataset = request.target_dataset_id;
        let mut data = collect_evidence(&self.owner, session, dataset, request.source_report_refs)?;
        let frozen = freeze_input_manifest(&self.contracts, json!({
            "dataset_id":dataset, "label_schema_digest":data.label_schema_digest,
            "capability_id":CAPABILITY,"implementation_version":"1","model_identity":null,
            "assets":data.members.iter().map(|member| manifest_asset(&member.scope)).collect::<Vec<_>>(),
            "resolved_config":intent,
        })).map_err(evidence)?;
        data.input_fingerprint = format_digest(&frozen.input_fingerprint);
        let root = self.owner.storage_root(session)?;
        let input_path = PathBuf::from(format!(
            "artifacts/task-inputs/{}.json",
            format_digest(&frozen.sha256)
        ));
        publish_immutable(&root, &input_path, &frozen.bytes)?;
        let submission = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(dataset),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: KIND.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: input_path,
            request_fingerprint: fingerprint,
            input_fingerprint: frozen.input_fingerprint,
        };
        self.owner.workspace_action(session, move |workspace| {
            for member in &data.members {
                let asset = member.scope.asset_id;
                let reported = workspace.connection.query_row(
                    "SELECT check_task_id FROM staging_items WHERE asset_id=?1 AND target_dataset_id=?2",
                    params![asset.to_string(), dataset.to_string()], |row| row.get::<_, Option<String>>(0),
                ).optional().map_err(database)?.flatten();
                let ArtifactOwner::Task(original_task) = member.source_report_ref.owner else {
                    return Ok(Err(StagingReportError::InvalidInput));
                };
                if reported.as_deref() != Some(original_task.to_string().as_str()) {
                    return Ok(Err(StagingReportError::InvalidInput));
                }
                let current = staged_image(workspace, dataset, asset)?;
                match verify_image(&current) {
                    Ok(true) if image_scope(&current) == member.scope => {}
                    Ok(_) => return Ok(Err(StagingReportError::InvalidInput)),
                    Err(error) => return Ok(Err(error.into())),
                }
            }
            Ok(seal_batch_report(workspace, &root, submission, data, now))
        })?
    }

    pub fn get_batch(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<StagingBatchReport, StagingReportError> {
        read_batch_report(&self.owner, session, task)
    }
}

fn collect_evidence(
    owner: &WorkspaceThread,
    session: Uuid,
    dataset: Uuid,
    references: Vec<ArtifactReference>,
) -> Result<BatchReportData, StagingReportError> {
    let mut data = BatchReportData {
        contract_version: 1,
        report_id: Uuid::new_v4(),
        dataset_id: dataset,
        label_schema_digest: String::new(),
        input_fingerprint: String::new(),
        members: Vec::new(),
        execution_coverage: BTreeMap::new(),
        findings_by_severity: BTreeMap::new(),
        unexecuted_checks: BTreeSet::new(),
    };
    for reference in references {
        let ArtifactOwner::Task(task) = reference.owner else {
            return Err(StagingReportError::InvalidInput);
        };
        let report = read_report(owner, session, task)?;
        if report.reference != reference || report.data.scope.dataset_id != dataset {
            return Err(StagingReportError::InvalidInput);
        }
        for (status, count) in report.data.execution_coverage {
            *data.execution_coverage.entry(status).or_default() += count;
        }
        for (severity, count) in report.data.findings_by_severity {
            *data.findings_by_severity.entry(severity).or_default() += count;
        }
        data.unexecuted_checks.extend(report.data.unexecuted_checks);
        data.members.push(BatchMember {
            scope: report.data.scope,
            source_report_ref: reference,
        });
    }
    data.members
        .sort_unstable_by_key(|member| member.scope.asset_id);
    data.label_schema_digest = data.members[0].scope.label_schema_digest.clone();
    if !valid_members(&data) {
        return Err(StagingReportError::InvalidInput);
    }
    Ok(data)
}

fn valid_members(data: &BatchReportData) -> bool {
    (2..=MAX_BATCH_ASSETS).contains(&data.members.len())
        && data
            .members
            .windows(2)
            .all(|pair| pair[0].scope.asset_id < pair[1].scope.asset_id)
        && data.members.iter().all(|member| {
            !member.scope.asset_id.is_nil()
                && member.scope.dataset_id == data.dataset_id
                && member.scope.label_schema_digest == data.label_schema_digest
        })
}

fn seal_batch_report(
    workspace: &mut crate::workspace::OpenedWorkspace,
    root: &Path,
    submission: TaskSubmission,
    data: BatchReportData,
    now: i64,
) -> Result<StagingBatchReport, StagingReportError> {
    let bytes = serde_json::to_vec(&data).map_err(evidence)?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return Err(StagingReportError::InvalidReport);
    }
    let transaction = workspace
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database)?;
    let task = enqueue_task_in(&transaction, &submission, now)
        .map_err(WorkspaceThreadError::Task)?
        .task_id;
    let path = report_path(task);
    publish_immutable(root, Path::new(&path), &bytes)?;
    let reference = ArtifactReference {
        artifact_id: data.report_id,
        owner: ArtifactOwner::Task(task),
        kind: BATCH_KIND.into(),
        media_type: "application/json".into(),
        sha256: format_digest(&Sha256::digest(&bytes).into()),
        size_bytes: bytes.len().to_string(),
        schema_id: None,
    };
    let metadata = json!({"terminal_receipt":{"result_manifest_ref":reference}}).to_string();
    let changed = transaction.execute(
        "UPDATE tasks SET status='SUCCEEDED',started_at_ms=?2,finished_at_ms=?2,updated_at_ms=?2,
            output_manifest_path=?3,execution_metadata_json=?4,row_revision=row_revision+1
         WHERE task_id=?1 AND status='QUEUED'",
        params![task.to_string(), now, path, metadata],
    ).map_err(database)?;
    if changed != 1 {
        return Err(StagingReportError::InvalidReport);
    }
    transaction.commit().map_err(database)?;
    Ok(result(task, reference, data))
}

pub(crate) fn read_batch_report(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
) -> Result<StagingBatchReport, StagingReportError> {
    let accepted = owner.workspace_action(session, move |workspace| {
        workspace.connection.query_row(
            "SELECT dataset_id,input_fingerprint,output_manifest_path,
                    json_extract(execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
             FROM tasks WHERE task_id=?1 AND operation_kind=?2 AND capability_id=?3 AND status='SUCCEEDED'",
            params![task.to_string(), KIND, CAPABILITY],
            |row| Ok((workspace.root().to_owned(), row_uuid(row, 0)?,
                row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
        ).optional().map_err(database)
    })?;
    let invalid = || StagingReportError::InvalidReport;
    let (root, dataset, fingerprint, path, encoded) = accepted.ok_or_else(invalid)?;
    let reference: ArtifactReference = serde_json::from_str(&encoded).map_err(evidence)?;
    let size: u64 = reference.size_bytes.parse().map_err(evidence)?;
    if path != report_path(task)
        || reference.owner != ArtifactOwner::Task(task)
        || reference.kind != BATCH_KIND
        || reference.media_type != "application/json"
        || size > MAX_REPORT_BYTES
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    open_scoped_file(&root, Path::new(&path))?
        .take(size + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size
        || format_digest(&Sha256::digest(&bytes).into()) != reference.sha256
    {
        return Err(invalid());
    }
    let data: BatchReportData =
        serde_json::from_value(strict_json(&bytes).map_err(evidence)?).map_err(evidence)?;
    if data.contract_version != 1
        || data.report_id != reference.artifact_id
        || data.input_fingerprint != fingerprint
        || data.dataset_id != dataset
        || !valid_members(&data)
    {
        return Err(invalid());
    }
    Ok(result(task, reference, data))
}

fn result(
    task_id: Uuid,
    reference: ArtifactReference,
    data: BatchReportData,
) -> StagingBatchReport {
    let asset_ids = data
        .members
        .iter()
        .map(|member| member.scope.asset_id)
        .collect();
    StagingBatchReport {
        task_id,
        reference,
        asset_ids,
        data,
    }
}

pub(crate) fn manifest_asset(scope: &ReportScope) -> serde_json::Value {
    json!({"asset_id":scope.asset_id,"media_digest":scope.media_digest,
        "annotation_digest":scope.annotation_digest,"source_metadata_digest":null})
}

fn evidence(error: impl std::error::Error + Send + Sync + 'static) -> StagingReportError {
    StagingReportError::Evidence(Box::new(error))
}
