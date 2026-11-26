//! Persist the Core's planning run and bind accepted plans to frozen input.

mod adjustment;
#[cfg(feature = "execution-rpc")]
mod geometry;
#[cfg(feature = "execution-rpc")]
mod local_runner;
#[cfg(feature = "execution-rpc")]
mod profile;
#[cfg(feature = "execution-rpc")]
mod report;
#[cfg(feature = "execution-rpc")]
pub use geometry::{WorkflowGeometryAcceptance, WorkflowGeometryRequest};
#[cfg(feature = "execution-rpc")]
pub use local_runner::{
    LocalHealthcheckAcceptance, LocalHealthcheckJob, LocalHealthcheckRequest,
    LocalHealthcheckRuntime,
};
#[cfg(feature = "execution-rpc")]
pub use profile::{WorkflowProfileAcceptance, WorkflowProfileRequest};
#[cfg(feature = "execution-rpc")]
pub use report::{WorkflowReport, WorkflowReportData, WorkflowReportRequest, WorkflowReportScope};

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dd_core::workflow::{
    AdjustmentError, AdjustmentRecord, ApprovalGate, DefaultSkipReason, EnabledStep, Fallback,
    PlanError, PlanPolicy, ScopeKind, Step, TemplatePlan, WorkflowInput, local_default_plan,
    template, validate_plan,
};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, JsonProfile};
use crate::paths::{is_portable_relative_file, open_scoped_file};
use crate::sqlite::{decode_uuid, row_uuid};
use crate::task_store::{TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const OPERATION: &str = "WORKFLOW_PLAN_PREVIEW";
const CAPABILITY: &str = "workflow_plan_preview.v1";
const PLANNING_NODE: &str = "plan";
const PROFILE_NODE: &str = "dataset_profile";
const GEOMETRY_NODE: &str = "annotation_geometry";
const REPORT_NODE: &str = "build_report";
const MANIFEST_LIMIT: u64 = 64 * 1024 * 1024;

pub struct WorkflowDraftRequest {
    pub dataset_id: Uuid,
    pub source: WorkflowInput,
    pub template_id: String,
    pub input_path: PathBuf,
    pub input_manifest_sha256: [u8; 32],
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanRef {
    pub run_id: Uuid,
    pub plan_revision: u64,
    pub input_fingerprint: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowStatus {
    Draft,
    Ready,
    Running,
    Interrupted,
    WaitingReview,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlanSource {
    Proposed,
    LocalDefault,
}

#[derive(Debug)]
pub struct WorkflowSnapshot {
    pub run_id: Uuid,
    pub planning_task_id: Uuid,
    pub status: WorkflowStatus,
    pub row_revision: u64,
    pub plan_ref: Option<PlanRef>,
    pub plan_source: Option<PlanSource>,
    pub not_executed: BTreeMap<Step, DefaultSkipReason>,
    pub adjustments: Vec<AdjustmentRecord>,
    pub profile_task: Option<WorkflowTaskSnapshot>,
    pub geometry_tasks: Vec<WorkflowGeometryTask>,
    pub report_task_id: Option<Uuid>,
    pub error_code: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct WorkflowTaskSnapshot {
    pub task_id: Uuid,
    pub status: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct WorkflowGeometryTask {
    pub asset_id: Uuid,
    pub task_id: Uuid,
    pub status: String,
}

#[derive(Debug)]
pub enum WorkflowError {
    Setup(Box<dyn Error + Send + Sync>),
    Storage(WorkspaceThreadError),
    Json(serde_json::Error),
    Plan(PlanError),
    Adjustment(AdjustmentError),
    Execution(Box<dyn Error + Send + Sync>),
    UpstreamNotReady,
    ExecutionControlRequired,
    InputChanged,
    InvalidInputScope,
    InvalidRecord,
    UnknownRun,
    RevisionConflict,
    IdempotencyConflict,
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "Initialize planning input contracts: {error}"),
            Self::Storage(error) => error.fmt(formatter),
            Self::Json(error) => write!(formatter, "Encode or read a workflow plan: {error}"),
            Self::Plan(error) => error.fmt(formatter),
            Self::Adjustment(error) => error.fmt(formatter),
            Self::Execution(error) => write!(formatter, "Prepare workflow execution: {error}"),
            Self::UpstreamNotReady => {
                formatter.write_str("The required upstream task has not succeeded")
            }
            Self::ExecutionControlRequired => {
                formatter.write_str("The adjustment requires an executing workflow node")
            }
            Self::InputChanged => {
                formatter.write_str("Workflow input no longer matches the frozen manifest")
            }
            Self::InvalidInputScope => {
                formatter.write_str("The template input scope is unavailable")
            }
            Self::InvalidRecord => {
                formatter.write_str("The Workspace contains an invalid workflow record")
            }
            Self::UnknownRun => {
                formatter.write_str("The workflow run does not exist in this Workspace")
            }
            Self::RevisionConflict => {
                formatter.write_str("The workflow run or plan revision changed")
            }
            Self::IdempotencyConflict => {
                formatter.write_str("The planning request fingerprint changed")
            }
        }
    }
}

impl Error for WorkflowError {}

pub struct WorkflowStore {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPlan {
    plan_revision: u64,
    plan: Option<PlanBody>,
    adjustment_count: u8,
    adjustments: Vec<AdjustmentRecord>,
    input: InputProof,
    #[serde(default)]
    plan_source: Option<PlanSource>,
    #[serde(default)]
    not_executed: BTreeMap<Step, DefaultSkipReason>,
}

struct PlanResolution {
    proposal: TemplatePlan,
    source: PlanSource,
    not_executed: BTreeMap<Step, DefaultSkipReason>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputProof {
    scope: ScopeKind,
    manifest_id: Uuid,
    input_fingerprint: String,
    manifest_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanBody {
    plan_kind: PlanKind,
    template_id: String,
    template_contract_version: u16,
    input_manifest_ref: Uuid,
    enabled_checks: Vec<Step>,
    typed_parameters_by_step: BTreeMap<Step, EnabledStep>,
    allowed_fallbacks_by_step: BTreeMap<Step, Vec<Fallback>>,
    max_adjustments: u8,
    required_review_gates: Vec<ApprovalGate>,
    expected_scope_fingerprint: String,
}

#[derive(Serialize, Deserialize)]
enum PlanKind {
    #[serde(rename = "template_instance")]
    TemplateInstance,
}

struct RowData {
    run_id: Uuid,
    planning_task_id: Uuid,
    dataset_id: Uuid,
    session: Uuid,
    template_key: String,
    status: String,
    row_revision: i64,
    input_path: String,
    plan_json: String,
    profile_task: Option<WorkflowTaskSnapshot>,
    #[cfg(feature = "execution-rpc")]
    node_states_json: String,
    geometry_tasks: Vec<WorkflowGeometryTask>,
    report_task_id: Option<Uuid>,
    error_code: Option<String>,
}

impl WorkflowStore {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, WorkflowError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(WorkflowError::Setup)?,
        })
    }

    pub fn admit(
        &self,
        session: Uuid,
        request: WorkflowDraftRequest,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let template = template(&request.template_id)
            .ok_or(WorkflowError::Plan(PlanError::UnregisteredTemplate))?;
        if template.scope != request.source.kind || template.scope != ScopeKind::Dataset {
            return Err(WorkflowError::InvalidInputScope);
        }
        let input_path = path_text(&request.input_path)?;
        let stored = StoredPlan {
            plan_revision: 0,
            plan: None,
            adjustment_count: 0,
            adjustments: Vec::new(),
            plan_source: None,
            not_executed: BTreeMap::new(),
            input: InputProof {
                scope: request.source.kind,
                manifest_id: request.source.manifest_id,
                input_fingerprint: format_digest(&request.source.fingerprint),
                manifest_sha256: format_digest(&request.input_manifest_sha256),
            },
        };
        let plan_json = serde_json::to_string(&stored).map_err(WorkflowError::Json)?;
        let digest_input = serde_json::to_vec(&(
            &request.template_id,
            request.dataset_id,
            &input_path,
            &stored.input,
        ))
        .map_err(WorkflowError::Json)?;
        let request_fingerprint = Sha256::digest(digest_input).into();
        let fingerprint = format_digest(&request_fingerprint);
        let key = request.idempotency_key;
        if let Some((id, original)) = self.find_by_key(session, key)? {
            if original != fingerprint {
                return Err(WorkflowError::IdempotencyConflict);
            }
            return self.get(session, id);
        }
        self.verify_input(session, request.dataset_id, &input_path, &stored.input)?;

        let run_id = Uuid::new_v4();
        let task = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(request.dataset_id),
            request_id: request.request_id,
            idempotency_key: key,
            operation_kind: OPERATION.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: PathBuf::from(&input_path),
            request_fingerprint,
            input_fingerprint: request.source.fingerprint,
        };
        let chosen = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            let inserted = transaction.execute(
                "INSERT INTO workflow_runs (
                    workflow_run_id,dataset_id,workspace_session_id,template_key,plan_kind,plan_json,
                    request_id,operation_kind,idempotency_key,request_fingerprint,input_manifest_path,
                    created_at_ms,updated_at_ms)
                 VALUES (?1,?2,?3,?4,'template_instance',?5,?6,?7,?8,?9,?10,?11,?11)
                 ON CONFLICT (operation_kind,idempotency_key) DO NOTHING",
                params![run_id.to_string(), request.dataset_id.to_string(), session.to_string(), request.template_id,
                        plan_json, request.request_id.to_string(), OPERATION, key.to_string(), fingerprint, input_path, now_ms],
            ).map_err(database)?;
            if inserted == 1 {
                let task = enqueue_task_in(&transaction, &task, now_ms).map_err(WorkspaceThreadError::Task)?;
                let attached = transaction.execute(
                    "UPDATE tasks SET workflow_run_id=?1,node_id=?2
                     WHERE task_id=?3 AND status='QUEUED' AND workflow_run_id IS NULL",
                    params![run_id.to_string(), PLANNING_NODE, task.task_id.to_string()],
                ).map_err(database)?;
                if attached != 1 {
                    return Ok(Err(WorkflowError::InvalidRecord));
                }
                transaction.commit().map_err(database)?;
                return Ok(Ok(run_id));
            }
            let (id, original): (Uuid, String) = transaction.query_row(
                "SELECT workflow_run_id,request_fingerprint FROM workflow_runs WHERE operation_kind=?1 AND idempotency_key=?2",
                params![OPERATION,key.to_string()], |row| Ok((row_uuid(row, 0)?, row.get(1)?)),
            ).map_err(database)?;
            Ok(if original == fingerprint { Ok(id) } else { Err(WorkflowError::IdempotencyConflict) })
        }).map_err(WorkflowError::Storage)??;
        self.get(session, chosen)
    }

    pub fn accept(
        &self,
        session: Uuid,
        run: Uuid,
        expected_row_revision: u64,
        policy: &PlanPolicy,
        proposal: TemplatePlan,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        self.resolve_draft(
            session,
            run,
            expected_row_revision,
            policy,
            now_ms,
            move |_, _| {
                Ok(PlanResolution {
                    proposal,
                    source: PlanSource::Proposed,
                    not_executed: BTreeMap::new(),
                })
            },
        )
    }

    pub fn accept_default(
        &self,
        session: Uuid,
        run: Uuid,
        expected_row_revision: u64,
        policy: &PlanPolicy,
        suggested_limit: u32,
        now_ms: i64,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        self.resolve_draft(
            session,
            run,
            expected_row_revision,
            policy,
            now_ms,
            |input, template_id| {
                let default = local_default_plan(template_id, input, policy, suggested_limit)
                    .map_err(WorkflowError::Plan)?;
                Ok(PlanResolution {
                    proposal: default.plan,
                    source: PlanSource::LocalDefault,
                    not_executed: default.not_executed,
                })
            },
        )
    }

    fn resolve_draft(
        &self,
        session: Uuid,
        run: Uuid,
        expected_row_revision: u64,
        policy: &PlanPolicy,
        now_ms: i64,
        resolve: impl FnOnce(&WorkflowInput, &str) -> Result<PlanResolution, WorkflowError>,
    ) -> Result<WorkflowSnapshot, WorkflowError> {
        let row = self.read_row(session, run)?;
        let mut stored = row.plan()?;
        if row.status != "DRAFT"
            || row.session != session
            || row.revision()? != expected_row_revision
        {
            return Err(WorkflowError::RevisionConflict);
        }
        let input = self.verify_input(session, row.dataset_id, &row.input_path, &stored.input)?;
        let resolution = resolve(&input, &row.template_key)?;
        if resolution.proposal.template_id != row.template_key {
            return Err(WorkflowError::Plan(PlanError::UnregisteredTemplate));
        }
        validate_plan(&input, policy, &resolution.proposal).map_err(WorkflowError::Plan)?;
        stored.plan = Some(PlanBody::from(resolution.proposal));
        stored.plan_source = Some(resolution.source);
        stored.not_executed = resolution.not_executed;
        stored.plan_revision = 1;
        let encoded = serde_json::to_string(&stored).map_err(WorkflowError::Json)?;
        let receipt = serde_json::json!({
            "workflow_run_id": run, "plan_revision": "1", "input_fingerprint": stored.input.input_fingerprint,
            "plan_source": resolution.source,
        }).to_string();
        let updated = self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            let updated = transaction.execute(
                "UPDATE workflow_runs SET status='READY',plan_json=?4,updated_at_ms=?5,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND workspace_session_id=?2 AND status='DRAFT' AND row_revision=?3",
                params![run.to_string(), session.to_string(), row.row_revision, encoded, now_ms],
            ).map_err(database)?;
            if updated != 1 {
                return Ok(false);
            }
            let completed = transaction.execute(
                "UPDATE tasks SET status='SUCCEEDED',execution_metadata_json=?4,
                    finished_at_ms=?5,updated_at_ms=?5,row_revision=row_revision+1
                 WHERE task_id=?1 AND workflow_run_id=?2 AND operation_kind=?3 AND status IN ('QUEUED','RUNNING')",
                params![row.planning_task_id.to_string(), run.to_string(), OPERATION, receipt, now_ms],
            ).map_err(database)?;
            if completed != 1 {
                return Ok(false);
            }
            transaction.commit().map_err(database)?;
            Ok(true)
        }).map_err(WorkflowError::Storage)?;
        if !updated {
            return Err(WorkflowError::RevisionConflict);
        }
        self.get(session, run)
    }

    pub fn get(&self, session: Uuid, run: Uuid) -> Result<WorkflowSnapshot, WorkflowError> {
        let mut row = self.read_row(session, run)?;
        if row.status == "RUNNING" && row.session != session {
            self.interrupt_ended_session(session, run)?;
            row = self.read_row(session, run)?;
        }
        let plan = row.plan()?;
        let status = match (
            row.status.as_str(),
            plan.plan_revision,
            plan.plan.is_some(),
            row.profile_task.is_some(),
            row.report_task_id.is_some(),
        ) {
            ("DRAFT", 0, false, false, false) => WorkflowStatus::Draft,
            ("READY", revision, true, false, false) if revision > 0 => WorkflowStatus::Ready,
            ("RUNNING", revision, true, true, false) if revision > 0 => WorkflowStatus::Running,
            ("INTERRUPTED", revision, true, true, false) if revision > 0 => {
                WorkflowStatus::Interrupted
            }
            ("WAITING_REVIEW", revision, true, true, true) if revision > 0 => {
                WorkflowStatus::WaitingReview
            }
            ("FAILED", revision, true, true, false) if revision > 0 => WorkflowStatus::Failed,
            _ => return Err(WorkflowError::InvalidRecord),
        };
        let plan_ref = if status != WorkflowStatus::Draft {
            Some(plan.reference(row.run_id)?)
        } else {
            None
        };
        Ok(WorkflowSnapshot {
            run_id: row.run_id,
            planning_task_id: row.planning_task_id,
            status,
            row_revision: row.revision()?,
            plan_ref,
            plan_source: (status != WorkflowStatus::Draft)
                .then(|| plan.plan_source.unwrap_or(PlanSource::Proposed)),
            not_executed: plan.not_executed,
            adjustments: plan.adjustments,
            profile_task: row.profile_task,
            geometry_tasks: row.geometry_tasks,
            report_task_id: row.report_task_id,
            error_code: row.error_code,
        })
    }

    fn interrupt_ended_session(&self, session: Uuid, run: Uuid) -> Result<(), WorkflowError> {
        self.owner
            .workspace_action(session, move |owner| {
                let transaction = owner.connection.transaction().map_err(database)?;
                let workflow = run.to_string();
                let session = session.to_string();
                let now: i64 = transaction
                    .query_row(
                        "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(database)?;
                let interrupted = transaction
                    .execute(
                        "UPDATE workflow_runs SET status='INTERRUPTED',error_code='SESSION_ENDED',
                    updated_at_ms=?3,row_revision=row_revision+1
                 WHERE workflow_run_id=?1 AND workspace_session_id<>?2 AND status='RUNNING'",
                        params![&workflow, &session, now],
                    )
                    .map_err(database)?;
                if interrupted == 1 {
                    transaction
                        .execute(
                            "UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',
                        finished_at_ms=?4,updated_at_ms=?4,row_revision=row_revision+1
                     WHERE workflow_run_id=?1 AND workspace_session_id<>?2 AND (node_id=?3 OR node_id GLOB ?5)
                       AND status IN ('QUEUED','DISPATCHING','RUNNING','CANCELLING')",
                    params![&workflow, &session, PROFILE_NODE, now, format!("{GEOMETRY_NODE}/*")],
                        )
                        .map_err(database)?;
                }
                transaction.commit().map_err(database)
            })
            .map_err(WorkflowError::Storage)
    }

    pub fn validate_ready(
        &self,
        session: Uuid,
        reference: &PlanRef,
        policy: &PlanPolicy,
    ) -> Result<TemplatePlan, WorkflowError> {
        let row = self.read_row(session, reference.run_id)?;
        let stored = row.plan()?;
        if row.status != "READY" || &stored.reference(reference.run_id)? != reference {
            return Err(WorkflowError::RevisionConflict);
        }
        let input = self.verify_input(session, row.dataset_id, &row.input_path, &stored.input)?;
        let plan = stored
            .plan
            .ok_or(WorkflowError::InvalidRecord)?
            .into_plan()?;
        if plan.template_id != row.template_key {
            return Err(WorkflowError::InvalidRecord);
        }
        validate_plan(&input, policy, &plan).map_err(WorkflowError::Plan)?;
        Ok(plan)
    }

    fn read_row(&self, session: Uuid, run: Uuid) -> Result<RowData, WorkflowError> {
        self.owner.workspace_action(session, move |owner| {
            let mut record = owner.connection.query_row(
                "SELECT w.workflow_run_id,w.dataset_id,w.workspace_session_id,w.template_key,w.status,w.row_revision,
                        w.input_manifest_path,w.plan_json,t.task_id,p.task_id,p.status,w.node_states_json,r.task_id,w.error_code
                 FROM workflow_runs w JOIN tasks t ON t.workflow_run_id=w.workflow_run_id AND t.node_id=?3
                    AND t.operation_kind=w.operation_kind
                 LEFT JOIN tasks p ON p.workflow_run_id=w.workflow_run_id AND p.node_id=?4
                 LEFT JOIN tasks r ON r.workflow_run_id=w.workflow_run_id AND r.node_id=?5
                    AND r.status='SUCCEEDED' AND r.output_manifest_path=w.result_manifest_path
                 WHERE w.workflow_run_id=?1 AND w.operation_kind=?2 AND w.plan_kind='template_instance'",
                params![run.to_string(),OPERATION,PLANNING_NODE,PROFILE_NODE,REPORT_NODE], decode_row,
            ).optional().map_err(database)?;
            if let Some(row) = &mut record && row.profile_task.is_some() {
                let mut statement = owner.connection.prepare(
                    "SELECT task_id,status,substr(node_id,?3) FROM tasks
                     WHERE workflow_run_id=?1 AND node_id GLOB ?2 AND operation_kind='ANNOTATION_GEOMETRY'
                     ORDER BY node_id",
                ).map_err(database)?;
                row.geometry_tasks = statement.query_map(
                    params![run.to_string(), format!("{GEOMETRY_NODE}/*"), GEOMETRY_NODE.len() as i64 + 2],
                    |task| Ok(WorkflowGeometryTask {
                        task_id: row_uuid(task, 0)?, status: task.get(1)?, asset_id: row_uuid(task, 2)?,
                    }),
                ).map_err(database)?.collect::<rusqlite::Result<Vec<_>>>().map_err(database)?;
            }
            Ok(record)
        }).map_err(WorkflowError::Storage)?.ok_or(WorkflowError::UnknownRun)
    }

    fn find_by_key(
        &self,
        session: Uuid,
        key: Uuid,
    ) -> Result<Option<(Uuid, String)>, WorkflowError> {
        self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT workflow_run_id,request_fingerprint FROM workflow_runs WHERE operation_kind=?1 AND idempotency_key=?2",
                params![OPERATION,key.to_string()], |row| Ok((row_uuid(row, 0)?, row.get(1)?)),
            ).optional().map_err(database)
        }).map_err(WorkflowError::Storage)
    }

    fn verify_input(
        &self,
        session: Uuid,
        dataset: Uuid,
        path: &str,
        proof: &InputProof,
    ) -> Result<WorkflowInput, WorkflowError> {
        self.verify_input_manifest(session, dataset, path, proof)
            .map(|(input, _)| input)
    }

    fn verify_input_manifest(
        &self,
        session: Uuid,
        dataset: Uuid,
        path: &str,
        proof: &InputProof,
    ) -> Result<(WorkflowInput, serde_json::Value), WorkflowError> {
        let path = Path::new(path);
        path_text(path)?;
        let root = self
            .owner
            .storage_root(session)
            .map_err(WorkflowError::Storage)?;
        let file = open_scoped_file(&root, path).map_err(|_| WorkflowError::InputChanged)?;
        let mut bytes = Vec::new();
        file.take(MANIFEST_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| WorkflowError::InputChanged)?;
        if bytes.len() as u64 > MANIFEST_LIMIT
            || format!("{:x}", Sha256::digest(&bytes)) != proof.manifest_sha256
        {
            return Err(WorkflowError::InputChanged);
        }
        let parsed = self
            .contracts
            .validate(JsonProfile::InputManifest, &bytes)
            .map_err(|_| WorkflowError::InputChanged)?;
        let canonical = freeze_input_manifest(&self.contracts, parsed.clone())
            .map_err(|_| WorkflowError::InputChanged)?;
        if bytes != canonical.bytes
            || format_digest(&canonical.input_fingerprint) != proof.input_fingerprint
            || parsed["dataset_id"] != dataset.to_string()
            || parsed["capability_id"] != CAPABILITY
            || proof.scope != ScopeKind::Dataset
            || proof.manifest_id.is_nil()
        {
            return Err(WorkflowError::InputChanged);
        }
        let dataset_key = dataset.to_string();
        let categories = self
            .owner
            .workspace_action(session, move |owner| {
                owner
                    .connection
                    .query_row(
                        "SELECT categories_json FROM datasets WHERE dataset_id=?1",
                        [&dataset_key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(database)
            })
            .map_err(WorkflowError::Storage)?
            .ok_or(WorkflowError::InputChanged)?;
        if parsed["label_schema_digest"] != format!("{:x}", Sha256::digest(categories.as_bytes())) {
            return Err(WorkflowError::InputChanged);
        }
        for entry in parsed["assets"]
            .as_array()
            .ok_or(WorkflowError::InputChanged)?
        {
            let asset = entry["asset_id"]
                .as_str()
                .and_then(|text| Uuid::parse_str(text).ok())
                .ok_or(WorkflowError::InputChanged)?;
            let registered = self
                .owner
                .registered_image(session, dataset, asset)
                .map_err(|error| match error {
                    WorkspaceThreadError::Dataset(_) => WorkflowError::InputChanged,
                    other => WorkflowError::Storage(other),
                })?;
            if entry["media_digest"] != registered.image.sha256
                || entry["annotation_digest"].as_str()
                    != registered
                        .annotation
                        .as_ref()
                        .map(|file| file.sha256.as_str())
                || !registered
                    .image
                    .live_digest(256 * 1024 * 1024)
                    .map_err(|_| WorkflowError::InputChanged)?
                || registered
                    .annotation
                    .as_ref()
                    .is_some_and(|file| file.live_digest(16 * 1024 * 1024).ok() != Some(true))
            {
                return Err(WorkflowError::InputChanged);
            }
        }
        Ok((
            WorkflowInput {
                kind: proof.scope,
                manifest_id: proof.manifest_id,
                fingerprint: canonical.input_fingerprint,
            },
            parsed,
        ))
    }
}

impl RowData {
    fn plan(&self) -> Result<StoredPlan, WorkflowError> {
        let stored: StoredPlan =
            serde_json::from_str(&self.plan_json).map_err(WorkflowError::Json)?;
        if usize::from(stored.adjustment_count) != stored.adjustments.len() {
            return Err(WorkflowError::InvalidRecord);
        }
        Ok(stored)
    }

    fn revision(&self) -> Result<u64, WorkflowError> {
        u64::try_from(self.row_revision).map_err(|_| WorkflowError::InvalidRecord)
    }
}

impl StoredPlan {
    fn reference(&self, run_id: Uuid) -> Result<PlanRef, WorkflowError> {
        Ok(PlanRef {
            run_id,
            plan_revision: self.plan_revision,
            input_fingerprint: digest_bytes(&self.input.input_fingerprint)?,
        })
    }
}

impl From<TemplatePlan> for PlanBody {
    fn from(plan: TemplatePlan) -> Self {
        Self {
            plan_kind: PlanKind::TemplateInstance,
            template_id: plan.template_id,
            template_contract_version: plan.template_contract_version,
            input_manifest_ref: plan.input_manifest_id,
            enabled_checks: plan.enabled_steps.iter().map(|step| step.step()).collect(),
            typed_parameters_by_step: plan
                .enabled_steps
                .into_iter()
                .map(|item| (item.step(), item))
                .collect(),
            allowed_fallbacks_by_step: plan.allowed_fallbacks,
            max_adjustments: plan.max_adjustments,
            required_review_gates: plan.required_review_gates,
            expected_scope_fingerprint: format_digest(&plan.expected_scope_fingerprint),
        }
    }
}

impl PlanBody {
    fn into_plan(mut self) -> Result<TemplatePlan, WorkflowError> {
        let enabled_steps = self
            .enabled_checks
            .into_iter()
            .map(|step| {
                self.typed_parameters_by_step
                    .remove(&step)
                    .filter(|item| item.step() == step)
                    .ok_or(WorkflowError::InvalidRecord)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !self.typed_parameters_by_step.is_empty() {
            return Err(WorkflowError::InvalidRecord);
        }
        Ok(TemplatePlan {
            template_id: self.template_id,
            template_contract_version: self.template_contract_version,
            input_manifest_id: self.input_manifest_ref,
            expected_scope_fingerprint: digest_bytes(&self.expected_scope_fingerprint)?,
            enabled_steps,
            allowed_fallbacks: self.allowed_fallbacks_by_step,
            max_adjustments: self.max_adjustments,
            required_review_gates: self.required_review_gates,
        })
    }
}

fn decode_row(row: &Row<'_>) -> rusqlite::Result<RowData> {
    let profile_task = match row.get::<_, Option<String>>(9)? {
        Some(id) => Some(WorkflowTaskSnapshot {
            task_id: decode_uuid(&id, 9)?,
            status: row.get(10)?,
        }),
        None => None,
    };
    Ok(RowData {
        run_id: row_uuid(row, 0)?,
        planning_task_id: row_uuid(row, 8)?,
        dataset_id: row_uuid(row, 1)?,
        session: row_uuid(row, 2)?,
        template_key: row.get(3)?,
        status: row.get(4)?,
        row_revision: row.get(5)?,
        input_path: row.get(6)?,
        plan_json: row.get(7)?,
        profile_task,
        #[cfg(feature = "execution-rpc")]
        node_states_json: row.get(11)?,
        geometry_tasks: Vec::new(),
        report_task_id: row
            .get::<_, Option<String>>(12)?
            .as_deref()
            .map(|id| decode_uuid(id, 12))
            .transpose()?,
        error_code: row.get(13)?,
    })
}

fn path_text(path: &Path) -> Result<String, WorkflowError> {
    if !path.starts_with("artifacts/task-inputs")
        || !is_portable_relative_file(path)
        || path.components().count() < 3
    {
        return Err(WorkflowError::InvalidInputScope);
    }
    Ok(path
        .iter()
        .map(|part| part.to_str().expect("checked UTF-8 path"))
        .collect::<Vec<_>>()
        .join("/"))
}

fn digest_bytes(text: &str) -> Result<[u8; 32], WorkflowError> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkflowError::InvalidRecord);
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| WorkflowError::InvalidRecord)?;
    }
    Ok(digest)
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::WorkflowDatabase(error)
}
