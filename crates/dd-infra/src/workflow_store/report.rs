//! Publish the authoritative health-check report and enter the human review gate.

mod checks;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use dd_core::workflow::{PlanPolicy, validate_plan};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::profile::verified_replay;
use super::{GEOMETRY_NODE, PlanRef, REPORT_NODE, WorkflowError, WorkflowStore, database};
use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::strict_json;
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::staging_report::ReportCheck;
use crate::task_store::{TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

const KIND: &str = "BUILD_REPORT";
const CAPABILITY: &str = "workflow_report.v1";
const ARTIFACT_KIND: &str = "WORKFLOW_CHECK_REPORT";
const LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct WorkflowReportRequest {
    pub reference: PlanRef,
    pub expected_row_revision: u64,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowReportScope {
    pub dataset_id: Uuid,
    pub workflow_run_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowReportData {
    pub contract_version: u32,
    pub report_id: Uuid,
    pub scope: WorkflowReportScope,
    pub input_fingerprint: String,
    pub execution_coverage: BTreeMap<String, u32>,
    pub findings_by_severity: BTreeMap<String, u32>,
    pub checks: Vec<ReportCheck>,
    pub unexecuted_checks: Vec<String>,
    pub repair_outcomes: Vec<Value>,
    pub human_ack_status: String,
}

#[derive(Debug, Serialize)]
pub struct WorkflowReport {
    pub task_id: Uuid,
    pub reference: ArtifactReference,
    pub data: WorkflowReportData,
}

impl WorkflowStore {
    pub fn build_report(
        &self,
        session: Uuid,
        policy: &PlanPolicy,
        request: WorkflowReportRequest,
        now_ms: i64,
    ) -> Result<WorkflowReport, WorkflowError> {
        let run = request.reference.run_id;
        let row = self.read_row(session, run)?;
        let stored = row.plan()?;
        if stored.reference(run)? != request.reference {
            return Err(WorkflowError::RevisionConflict);
        }
        let request_fingerprint = digest_request(&request);
        let fingerprint = format_digest(&request_fingerprint);
        let key = request.idempotency_key.to_string();
        if row.report_task_id.is_some() {
            let key = key.clone();
            self.owner
                .workspace_action(session, move |owner| {
                    let receipt = report_command(&owner.connection, run)?;
                    Ok(verified_replay(receipt, &key, &fingerprint))
                })
                .map_err(WorkflowError::Storage)??;
            return self.read_report(session, run);
        }
        if row.status != "RUNNING"
            || row.session != session
            || row.revision()? != request.expected_row_revision
        {
            return Err(WorkflowError::RevisionConflict);
        }
        let mut nodes: Value =
            serde_json::from_str(&row.node_states_json).map_err(WorkflowError::Json)?;
        let bindings = nodes["nodes"]
            .as_array()
            .ok_or(WorkflowError::InvalidRecord)?;
        if !bindings.iter().any(|node| node["node_id"] == GEOMETRY_NODE)
            || row.geometry_tasks.iter().any(|task| {
                matches!(
                    task.status.as_str(),
                    "QUEUED" | "DISPATCHING" | "RUNNING" | "CANCELLING"
                )
            })
        {
            return Err(WorkflowError::UpstreamNotReady);
        }
        let (source, mut scope) =
            self.verify_input_manifest(session, row.dataset_id, &row.input_path, &stored.input)?;
        let plan = stored
            .plan
            .ok_or(WorkflowError::InvalidRecord)?
            .into_plan()?;
        if plan.template_id != row.template_key {
            return Err(WorkflowError::InvalidRecord);
        }
        validate_plan(&source, policy, &plan).map_err(WorkflowError::Plan)?;
        let checks = checks::collect(
            &self.owner,
            session,
            &row,
            &plan,
            &stored.not_executed,
            &scope,
        )?;
        let refs: Vec<_> = checks
            .iter()
            .flat_map(|check| &check.evidence_refs)
            .collect();
        scope["capability_id"] = json!(CAPABILITY);
        scope["implementation_version"] = json!(env!("CARGO_PKG_VERSION"));
        scope["resolved_config"] = json!({
            "workflow_run_id":run, "plan_revision":request.reference.plan_revision,
            "plan_scope_fingerprint":format_digest(&request.reference.input_fingerprint), "evidence_refs":refs,
        });
        let manifest = freeze_input_manifest(&self.contracts, scope).map_err(evidence)?;
        let input_path = PathBuf::from(format!(
            "artifacts/task-inputs/{}.json",
            format_digest(&manifest.sha256)
        ));
        let root = self
            .owner
            .storage_root(session)
            .map_err(WorkflowError::Storage)?;
        publish_immutable(&root, &input_path, &manifest.bytes).map_err(evidence)?;
        let submission = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(row.dataset_id),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: KIND.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: input_path,
            request_fingerprint,
            input_fingerprint: manifest.input_fingerprint,
        };
        let report = summarize(
            run,
            row.dataset_id,
            format_digest(&manifest.input_fingerprint),
            checks,
        );
        let bytes = serde_json::to_vec(&report).map_err(WorkflowError::Json)?;
        if bytes.len() as u64 > LIMIT {
            return Err(WorkflowError::InvalidRecord);
        }
        self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            if let Some(receipt) = report_command(&transaction, run)? {
                return Ok(verified_replay(Some(receipt), &key, &fingerprint));
            }
            let task = enqueue_task_in(&transaction, &submission, now_ms).map_err(WorkspaceThreadError::Task)?;
            let output = report_path(task.task_id);
            if let Err(error) = publish_immutable(&root, Path::new(&output), &bytes) {
                return Ok(Err(evidence(error)));
            }
            let reference = ArtifactReference {
                artifact_id:report.report_id, owner:ArtifactOwner::Task(task.task_id), kind:ARTIFACT_KIND.into(),
                media_type:"application/json".into(), sha256:format!("{:x}", Sha256::digest(&bytes)),
                size_bytes:bytes.len().to_string(), schema_id:None,
            };
            let metadata = json!({"terminal_receipt":{"result_manifest_ref":reference}}).to_string();
            let completed = transaction.execute(
                "UPDATE tasks SET status='SUCCEEDED',workflow_run_id=?2,node_id=?3,output_manifest_path=?4,
                    execution_metadata_json=?5,started_at_ms=?6,finished_at_ms=?6,updated_at_ms=?6,
                    row_revision=row_revision+1
                 WHERE task_id=?1 AND status='QUEUED' AND workflow_run_id IS NULL",
                params![task.task_id.to_string(),run.to_string(),REPORT_NODE,&output,metadata,now_ms],
            ).map_err(database)?;
            let Some(bindings) = nodes["nodes"].as_array_mut() else { return Ok(Err(WorkflowError::InvalidRecord)); };
            bindings.push(json!({"node_id":REPORT_NODE,"stage_id":"report","task_id":task.task_id}));
            bindings.push(json!({"node_id":"final_annotation","stage_id":"review","status":"WAITING_REVIEW"}));
            let waiting = transaction.execute(
                "UPDATE workflow_runs SET status='WAITING_REVIEW',result_manifest_path=?4,node_states_json=?5,
                    updated_at_ms=?6,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND status='RUNNING' AND row_revision=?2 AND workspace_session_id=?3",
                params![run.to_string(),row.row_revision,session.to_string(),&output,nodes.to_string(),now_ms],
            ).map_err(database)?;
            if completed != 1 || waiting != 1 {
                return Ok(Err(WorkflowError::RevisionConflict));
            }
            transaction.commit().map_err(database)?;
            Ok(Ok(()))
        }).map_err(WorkflowError::Storage)??;
        self.read_report(session, run)
    }

    pub fn read_report(&self, session: Uuid, run: Uuid) -> Result<WorkflowReport, WorkflowError> {
        let accepted = self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT t.task_id,w.dataset_id,t.input_fingerprint,t.output_manifest_path,
                        json_extract(t.execution_metadata_json,'$.terminal_receipt.result_manifest_ref')
                 FROM workflow_runs w JOIN tasks t ON t.workflow_run_id=w.workflow_run_id
                 WHERE w.workflow_run_id=?1 AND t.node_id=?2 AND t.operation_kind=?3 AND t.capability_id=?4
                   AND t.status='SUCCEEDED' AND w.result_manifest_path=t.output_manifest_path",
                params![run.to_string(), REPORT_NODE, KIND, CAPABILITY],
                |row| Ok((owner.root().to_owned(),row_uuid(row,0)?,row_uuid(row,1)?,
                    row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?)),
            ).optional().map_err(database)
        }).map_err(WorkflowError::Storage)?.ok_or(WorkflowError::UpstreamNotReady)?;
        let (root, task_id, dataset_id, fingerprint, path, receipt) = accepted;
        let reference: ArtifactReference =
            serde_json::from_str(&receipt).map_err(WorkflowError::Json)?;
        let size: u64 = reference.size_bytes.parse().map_err(evidence)?;
        if reference.owner != ArtifactOwner::Task(task_id)
            || reference.kind != ARTIFACT_KIND
            || reference.media_type != "application/json"
            || size > LIMIT
            || path != report_path(task_id)
        {
            return Err(WorkflowError::InvalidRecord);
        }
        let mut bytes = Vec::new();
        open_scoped_file(&root, Path::new(&path))
            .map_err(evidence)?
            .take(size + 1)
            .read_to_end(&mut bytes)
            .map_err(evidence)?;
        if bytes.len() as u64 != size || format!("{:x}", Sha256::digest(&bytes)) != reference.sha256
        {
            return Err(WorkflowError::InvalidRecord);
        }
        let data: WorkflowReportData =
            serde_json::from_value(strict_json(&bytes).map_err(evidence)?)
                .map_err(WorkflowError::Json)?;
        if data.contract_version != 1
            || data.report_id != reference.artifact_id
            || data.scope.dataset_id != dataset_id
            || data.scope.workflow_run_id != run
            || data.input_fingerprint != fingerprint
        {
            return Err(WorkflowError::InvalidRecord);
        }
        Ok(WorkflowReport {
            task_id,
            reference,
            data,
        })
    }
}

fn summarize(
    run: Uuid,
    dataset: Uuid,
    input_fingerprint: String,
    checks: Vec<ReportCheck>,
) -> WorkflowReportData {
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
    WorkflowReportData {
        contract_version: 1,
        report_id: Uuid::new_v4(),
        scope: WorkflowReportScope {
            dataset_id: dataset,
            workflow_run_id: run,
        },
        input_fingerprint,
        execution_coverage,
        findings_by_severity,
        checks,
        unexecuted_checks,
        repair_outcomes: Vec::new(),
        human_ack_status: "PENDING".into(),
    }
}

fn report_path(task: Uuid) -> String {
    format!("artifacts/tasks/{task}/report.json")
}

fn digest_request(request: &WorkflowReportRequest) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"datasetdoctor.workflow.build-report.v1\0");
    digest.update(request.reference.run_id.as_bytes());
    digest.update(request.reference.input_fingerprint);
    digest.update(request.reference.plan_revision.to_le_bytes());
    digest.update(request.expected_row_revision.to_le_bytes());
    digest.finalize().into()
}

fn report_command(
    connection: &Connection,
    run: Uuid,
) -> Result<Option<(String, String)>, WorkspaceThreadError> {
    connection.query_row(
        "SELECT idempotency_key,request_fingerprint FROM tasks WHERE workflow_run_id=?1 AND node_id=?2 AND capability_id=?3",
        params![run.to_string(),REPORT_NODE,CAPABILITY], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional().map_err(database)
}

fn evidence(error: impl std::error::Error + Send + Sync + 'static) -> WorkflowError {
    WorkflowError::Execution(Box::new(error))
}
