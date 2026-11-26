//! Export a verified report to a directory selected through the local UI.

mod publication;

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use self::publication::{Publication, recover_publication, write_export};
use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, strict_json};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::staging_report;
use crate::task_store::{TaskStoreError, TaskSubmission, format_digest};
use crate::worker_runtime::now_ms;
use crate::workflow_store::WorkflowStore;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const OPERATION: &str = "REPORT_EXPORT";
const CAPABILITY: &str = "report_export.v1";
const REPORT_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReportExportFormat {
    Json,
    Markdown,
}

impl ReportExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "md",
        }
    }
}

#[derive(Clone)]
pub struct ReportExportRequest {
    pub report_ref: ArtifactReference,
    pub format: ReportExportFormat,
    pub destination_grant_id: Uuid,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

pub struct ReportExportJob {
    task: Uuid,
    session: Uuid,
    destination: PathBuf,
    report_ref: ArtifactReference,
    format: ReportExportFormat,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportExportReceipt {
    pub report_ref: ArtifactReference,
    pub format: ReportExportFormat,
    pub file_name: String,
    pub sha256: String,
    pub size_bytes: String,
}

#[derive(Debug)]
pub struct ReportExportTask {
    pub task_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub accepted_at_ms: i64,
    pub receipt: Option<ReportExportReceipt>,
}

#[derive(Debug)]
pub enum ReportExportError {
    Workspace(WorkspaceThreadError),
    Io(io::Error),
    Json(serde_json::Error),
    Evidence(Box<dyn std::error::Error + Send + Sync>),
    InvalidReport,
    InvalidTask,
}

impl std::fmt::Display for ReportExportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "Access report export: {error}"),
            Self::Json(error) => write!(formatter, "Encode report export: {error}"),
            Self::Evidence(error) => write!(formatter, "Verify report export evidence: {error}"),
            Self::InvalidReport => {
                formatter.write_str("Report reference does not match accepted evidence")
            }
            Self::InvalidTask => formatter.write_str("Report export task is unavailable"),
        }
    }
}

impl std::error::Error for ReportExportError {}

impl From<WorkspaceThreadError> for ReportExportError {
    fn from(error: WorkspaceThreadError) -> Self {
        Self::Workspace(error)
    }
}

impl From<io::Error> for ReportExportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ReportExportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub struct ReportExportService {
    owner: Arc<WorkspaceThread>,
    workflows: WorkflowStore,
    contracts: JsonContracts,
}

impl ReportExportService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, ReportExportError> {
        Ok(Self {
            workflows: WorkflowStore::new(Arc::clone(&owner)).map_err(evidence)?,
            contracts: JsonContracts::new().map_err(ReportExportError::Evidence)?,
            owner,
        })
    }

    pub fn submit(
        &self,
        session: Uuid,
        request: ReportExportRequest,
    ) -> Result<(ReportExportTask, Option<ReportExportJob>), ReportExportError> {
        let _guard = self.owner.lock_final_operations()?;
        let fingerprint: [u8; 32] = Sha256::digest(serde_json::to_vec(&(
            CAPABILITY,
            &request.report_ref,
            request.format,
            request.destination_grant_id,
        ))?)
        .into();
        let key = request.idempotency_key.to_string();
        let existing = self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT task_id,request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
                params![OPERATION, key],
                |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
            ).optional().map_err(database)
        })?;
        if let Some((task, accepted)) = existing {
            if accepted != format_digest(&fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return Ok((self.get_locked(session, task)?, None));
        }

        let source = self.source(session, &request.report_ref)?;
        let frozen = freeze_input_manifest(
            &self.contracts,
            json!({
                "dataset_id": source.dataset, "label_schema_digest": null, "assets": [],
                "capability_id": CAPABILITY, "implementation_version": env!("CARGO_PKG_VERSION"),
                "model_identity": null, "resolved_config": {
                    "report_ref": request.report_ref, "format": request.format,
                },
            }),
        )
        .map_err(evidence)?;
        let destination = self
            .owner
            .consume_export_directory(session, request.destination_grant_id)?;
        let path = Path::new("artifacts/task-inputs")
            .join(format!("{}.json", format_digest(&frozen.sha256)));
        publish_immutable(&self.owner.storage_root(session)?, &path, &frozen.bytes)?;
        let task = self.owner.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: Some(source.dataset),
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: OPERATION.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: path,
                request_fingerprint: fingerprint,
                input_fingerprint: frozen.input_fingerprint,
            },
            now_ms().map_err(evidence)?,
        )?;
        Ok((
            ReportExportTask {
                task_id: task.task_id,
                status: task.status,
                error_code: None,
                accepted_at_ms: task.created_at_ms,
                receipt: None,
            },
            Some(ReportExportJob {
                task: task.task_id,
                session,
                destination,
                report_ref: request.report_ref,
                format: request.format,
            }),
        ))
    }

    pub fn run(&self, session: Uuid, job: ReportExportJob) -> Result<(), ReportExportError> {
        if session != job.session {
            return Err(ReportExportError::InvalidTask);
        }
        let _guard = self.owner.lock_final_operations()?;
        let now = now_ms().map_err(evidence)?;
        let task = job.task;
        let claimed = self.owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "UPDATE tasks SET status='RUNNING', started_at_ms=?4,updated_at_ms=?4,row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'",
                params![task.to_string(), OPERATION, session.to_string(), now],
            ).map_err(database)
        })?;
        if claimed != 1 {
            return Err(ReportExportError::InvalidTask);
        }

        let result = self.export(session, job);
        match result {
            Ok(receipt) => {
                publication::finish(&self.owner, session, task, "SUCCEEDED", None, Some(receipt))
            }
            Err(error) => {
                let code = match &error {
                    ReportExportError::InvalidReport => "REPORT_EVIDENCE_CHANGED",
                    ReportExportError::Io(_) => "REPORT_OUTPUT_UNAVAILABLE",
                    _ => "REPORT_EXPORT_FAILED",
                };
                publication::finish(&self.owner, session, task, "FAILED", Some(code), None)?;
                Err(error)
            }
        }
    }

    pub fn get(&self, session: Uuid, task: Uuid) -> Result<ReportExportTask, ReportExportError> {
        let _guard = self.owner.lock_final_operations()?;
        self.get_locked(session, task)
    }

    fn get_locked(&self, session: Uuid, task: Uuid) -> Result<ReportExportTask, ReportExportError> {
        recover_publication(&self.owner, session, task)?;
        let now = now_ms().map_err(evidence)?;
        let row = self.owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',finished_at_ms=?4,
                    updated_at_ms=?4,row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3
                    AND status IN ('QUEUED','RUNNING')",
                params![task.to_string(), OPERATION, session.to_string(), now],
            ).map_err(database)?;
            owner
                .connection
                .query_row(
                    "SELECT status,error_code,execution_metadata_json,created_at_ms FROM tasks
                 WHERE task_id=?1 AND operation_kind=?2 AND capability_id=?3",
                    params![task.to_string(), OPERATION, CAPABILITY],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get(1)?,
                            row.get::<_, String>(2)?,
                            row.get(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(database)
        })?;
        let (status, error_code, metadata, accepted_at_ms) = row.ok_or(ReportExportError::InvalidTask)?;
        let receipt = if status == "SUCCEEDED" {
            Some(serde_json::from_str(&metadata)?)
        } else {
            None
        };
        Ok(ReportExportTask {
            task_id: task,
            status,
            error_code,
            accepted_at_ms,
            receipt,
        })
    }

    fn export(
        &self,
        session: Uuid,
        job: ReportExportJob,
    ) -> Result<ReportExportReceipt, ReportExportError> {
        let ReportSource { bytes, .. } = self.source(session, &job.report_ref)?;
        let output = match job.format {
            ReportExportFormat::Json => bytes,
            ReportExportFormat::Markdown => render_markdown(&job.report_ref, &bytes)?.into_bytes(),
        };
        let publication = Publication {
            destination: job.destination,
            receipt: ReportExportReceipt {
                report_ref: job.report_ref,
                format: job.format,
                file_name: file_name(job.task, job.format),
                sha256: format_digest(&Sha256::digest(&output).into()),
                size_bytes: output.len().to_string(),
            },
        };
        let staged = publication.stage(&output)?;
        publication.prepare(&self.owner, session, job.task)?;
        write_export(
            &publication.destination,
            &publication.receipt.file_name,
            staged,
        )?;
        Ok(publication.receipt)
    }

    fn source(
        &self,
        session: Uuid,
        reference: &ArtifactReference,
    ) -> Result<ReportSource, ReportExportError> {
        let ArtifactOwner::Task(task) = reference.owner else {
            return Err(ReportExportError::InvalidReport);
        };
        let (root, dataset, workflow, path) = self
            .owner
            .workspace_action(session, move |owner| {
                owner
                    .connection
                    .query_row(
                        "SELECT dataset_id,workflow_run_id,output_manifest_path FROM tasks
                 WHERE task_id=?1 AND operation_kind='BUILD_REPORT' AND status='SUCCEEDED'",
                        params![task.to_string()],
                        |row| {
                            Ok((
                                owner.root().to_owned(),
                                row_uuid(row, 0)?,
                                row.get::<_, Option<String>>(1)?,
                                PathBuf::from(row.get::<_, String>(2)?),
                            ))
                        },
                    )
                    .optional()
                    .map_err(database)
            })?
            .ok_or(ReportExportError::InvalidReport)?;
        let (accepted_dataset, accepted) = match workflow {
            Some(run) => {
                let report = self
                    .workflows
                    .read_report(session, run.parse().map_err(evidence)?)
                    .map_err(evidence)?;
                (report.data.scope.dataset_id, report.reference)
            }
            None => {
                let report =
                    staging_report::read_report(&self.owner, session, task).map_err(evidence)?;
                (report.data.scope.dataset_id, report.reference)
            }
        };
        if accepted != *reference || accepted_dataset != dataset {
            return Err(ReportExportError::InvalidReport);
        }
        let size: u64 = accepted.size_bytes.parse().map_err(evidence)?;
        if size > REPORT_LIMIT {
            return Err(ReportExportError::InvalidReport);
        }
        let mut bytes = Vec::new();
        open_scoped_file(&root, &path)?
            .take(size + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size
            || format_digest(&Sha256::digest(&bytes).into()) != accepted.sha256
        {
            return Err(ReportExportError::InvalidReport);
        }
        Ok(ReportSource { dataset, bytes })
    }
}

struct ReportSource {
    dataset: Uuid,
    bytes: Vec<u8>,
}

fn file_name(task: Uuid, format: ReportExportFormat) -> String {
    format!("report-{task}.{}", format.extension())
}

fn render_markdown(
    reference: &ArtifactReference,
    bytes: &[u8],
) -> Result<String, ReportExportError> {
    let data = strict_json(bytes).map_err(evidence)?;
    let checks = data["checks"]
        .as_array()
        .ok_or(ReportExportError::InvalidReport)?;
    let mut markdown = format!(
        "# Dataset Doctor report\n\nReport ID `{}`\n\nSource SHA-256 `{}`\n\n## Checks\n\n",
        reference.artifact_id, reference.sha256,
    );
    for check in checks {
        let field = |key: &str| check[key].as_str().ok_or(ReportExportError::InvalidReport);
        let id = field("check_id")?;
        let status = field("execution_status")?;
        let assessment = field("assessment")?;
        let reason = check["skip_or_failure_reason"]["code"]
            .as_str()
            .unwrap_or("");
        markdown.push_str(&format!(
            "- `{}` · `{}` · `{}`  {}\n",
            markdown_token(id),
            markdown_token(status),
            markdown_token(assessment),
            markdown_token(reason),
        ));
    }
    let document = serde_json::to_string::<Value>(&data)?;
    markdown.push_str("\n## Full report\n\n```json\n");
    markdown.push_str(&document);
    markdown.push_str("\n```\n");
    Ok(markdown)
}

fn markdown_token(value: &str) -> String {
    value
        .chars()
        .filter(|value| value.is_ascii_alphanumeric() || "_-.".contains(*value))
        .collect()
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}

fn evidence(error: impl std::error::Error + Send + Sync + 'static) -> ReportExportError {
    ReportExportError::Evidence(Box::new(error))
}
