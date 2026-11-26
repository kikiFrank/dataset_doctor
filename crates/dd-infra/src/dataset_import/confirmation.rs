//! Durable Core tasks for applying an approved import plan.

use std::path::PathBuf;

use dd_core::import_plan::Approval;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    DatasetImportCoordinator, DatasetImportError, ImportRegistrationError, prepare_registration,
};
use crate::import_registration::{ImportRegistration, register_import};
use crate::input_fingerprint::freeze_input_manifest;
use crate::paths::publish_immutable;
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

const OPERATION: &str = "DATASET_IMPORT_EXTERNAL";
const CAPABILITY: &str = "dataset_import_external.v1";

#[derive(Clone, Copy, Debug)]
pub struct ImportConfirmation {
    pub plan_id: Uuid,
    pub plan_fingerprint: [u8; 32],
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

impl ImportConfirmation {
    fn request_fingerprint(&self) -> [u8; 32] {
        let mut sha = Sha256::new();
        sha.update(b"datasetdoctor.dataset_import_external.v1\0");
        sha.update(self.plan_id.as_bytes());
        sha.update(self.plan_fingerprint);
        sha.finalize().into()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ImportTaskSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub accepted_at_ms: i64,
    pub registration: Option<ImportRegistration>,
    pub error_code: Option<String>,
}

impl DatasetImportCoordinator {
    pub fn submit_confirmation(
        &self,
        session: Uuid,
        request: ImportConfirmation,
        now_ms: i64,
    ) -> Result<ImportTaskSnapshot, DatasetImportError> {
        let fingerprint = request.request_fingerprint();
        if let Some(task_id) = self
            .workspace
            .workspace_action(session, move |workspace| {
                find_request(
                    &workspace.connection,
                    OPERATION,
                    request.idempotency_key,
                    &fingerprint,
                )
            })
            .map_err(DatasetImportError::Workspace)?
        {
            return self.confirmation_task(session, task_id, now_ms);
        }

        let approvals = self
            .approvals
            .lock()
            .map_err(|_| DatasetImportError::Unavailable)?;
        // Recheck after taking the plan lock, since another caller may have
        // persisted and started this task while this request was waiting.
        if let Some(task_id) = self
            .workspace
            .workspace_action(session, move |workspace| {
                find_request(
                    &workspace.connection,
                    OPERATION,
                    request.idempotency_key,
                    &fingerprint,
                )
            })
            .map_err(DatasetImportError::Workspace)?
        {
            return self.confirmation_task(session, task_id, now_ms);
        }
        let plan = approvals
            .preview(session, request.plan_id, request.plan_fingerprint)
            .map_err(DatasetImportError::Approval)?;
        let manifest = freeze_input_manifest(
            &self.contracts,
            json!({
                "capability_id": CAPABILITY,
                "implementation_version": "1",
                "dataset_id": null,
                "label_schema_digest": null,
                "assets": [],
                "model_identity": null,
                "resolved_config": {
                    "plan_id": request.plan_id.to_string(),
                    "plan_fingerprint": format_digest(&request.plan_fingerprint),
                    "image_count": plan.preview.inputs.len(),
                },
            }),
        )
        .map_err(|error| DatasetImportError::Source(std::io::Error::other(error)))?;
        let manifest_path = PathBuf::from(format!(
            "artifacts/import/{}.json",
            format_digest(&manifest.sha256)
        ));
        let root = self
            .workspace
            .storage_root(session)
            .map_err(DatasetImportError::Workspace)?;
        publish_immutable(&root, &manifest_path, &manifest.bytes)
            .map_err(DatasetImportError::Source)?;
        let task_id = self.workspace.workspace_action(session, move |workspace| {
            let duplicate: bool = workspace.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE operation_kind=?1 AND request_fingerprint=?2 AND idempotency_key<>?3)",
                params![OPERATION, format_digest(&fingerprint), request.idempotency_key.to_string()],
                |row| row.get(0),
            ).map_err(database)?;
            if duplicate {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict));
            }
            workspace.enqueue_task(&TaskSubmission {
                workspace_session: session,
                dataset_id: None,
                request_id: request.request_id,
                idempotency_key: request.idempotency_key,
                operation_kind: OPERATION.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: manifest_path,
                request_fingerprint: fingerprint,
                input_fingerprint: manifest.input_fingerprint,
            }, now_ms).map(|task| task.task_id).map_err(WorkspaceThreadError::Task)
        }).map_err(DatasetImportError::Workspace)?;
        drop(approvals);
        self.confirmation_task(session, task_id, now_ms)
    }

    /// Reconcile a previously queued apply when a new Workspace session opens.
    /// External paths are reauthorized through a new preview after interruption.
    pub fn confirmation_task(
        &self,
        session: Uuid,
        task_id: Uuid,
        now_ms: i64,
    ) -> Result<ImportTaskSnapshot, DatasetImportError> {
        self.workspace
            .workspace_action(session, move |workspace| {
                workspace
                    .connection
                    .execute(
                        "UPDATE tasks SET status='INTERRUPTED', error_code='SESSION_ENDED',
                    finished_at_ms=?4, updated_at_ms=?4, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3
                   AND status IN ('QUEUED', 'RUNNING')",
                        params![task_id.to_string(), OPERATION, session.to_string(), now_ms],
                    )
                    .map_err(database)?;
                read_snapshot(&workspace.connection, task_id)
            })
            .map_err(DatasetImportError::Workspace)
    }

    /// Execute on an I/O thread. Only the task claimant rehashes inputs.
    pub fn run_confirmation(
        &self,
        session: Uuid,
        request: ImportConfirmation,
        now_ms: i64,
    ) -> Result<ImportTaskSnapshot, DatasetImportError> {
        let current = self.submit_confirmation(session, request, now_ms)?;
        if current.status != "QUEUED" {
            return Ok(current);
        }
        let task_id = current.task_id;
        let attempt_id = Uuid::new_v4();
        let claimed = self.workspace.workspace_action(session, move |workspace| {
            workspace.connection.execute(
                "UPDATE tasks SET status='RUNNING', current_attempt_id=?4, attempt_no=attempt_no+1,
                    started_at_ms=?5, updated_at_ms=?5, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'",
                params![task_id.to_string(), OPERATION, session.to_string(), attempt_id.to_string(), now_ms],
            ).map_err(database)
        }).map_err(DatasetImportError::Workspace)?;
        if claimed == 0 {
            return self.confirmation_task(session, task_id, now_ms);
        }

        let approval = self
            .approvals
            .lock()
            .map_err(|_| DatasetImportError::Unavailable)?
            .confirm(session, request.plan_id, request.plan_fingerprint);
        let Approval::Apply(plan) = (match approval {
            Ok(approval) => approval,
            Err(_) => {
                return self.fail_confirmation(
                    session,
                    task_id,
                    attempt_id,
                    now_ms,
                    "APPROVAL_UNAVAILABLE",
                );
            }
        }) else {
            return self.fail_confirmation(
                session,
                task_id,
                attempt_id,
                now_ms,
                "APPROVAL_UNAVAILABLE",
            );
        };
        let receipt = prepare_registration(&self.contracts, &plan.dataset_name, &plan.preview)
            .map_err(DatasetImportError::Registration)
            .and_then(|prepared| {
                self.workspace.workspace_action(session, move |workspace| {
                    let tx = workspace.connection.transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(database)?;
                    let receipt = register_import(&tx, prepared, now_ms).map_err(WorkspaceThreadError::Import)?;
                    let encoded = serde_json::to_string(&receipt)
                        .map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Receipt(error)))?;
                    let updated = tx.execute(
                        "UPDATE tasks SET status='SUCCEEDED', dataset_id=?5, progress=1,
                            execution_metadata_json=json_set(execution_metadata_json, '$.import_receipt', json(?6)),
                            finished_at_ms=?7, updated_at_ms=?7, row_revision=row_revision+1
                         WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3
                           AND current_attempt_id=?4 AND status='RUNNING'",
                        params![task_id.to_string(), OPERATION, session.to_string(), attempt_id.to_string(),
                            receipt.dataset_id, encoded, now_ms],
                    ).map_err(database)?;
                    if updated != 1 {
                        return Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict));
                    }
                    tx.commit().map_err(database)?;
                    Ok(receipt)
                }).map_err(|error| match error {
                    WorkspaceThreadError::Import(error) => DatasetImportError::Registration(error),
                    other => DatasetImportError::Workspace(other),
                })
            });
        let mut approvals = self
            .approvals
            .lock()
            .map_err(|_| DatasetImportError::Unavailable)?;
        match receipt {
            Ok(receipt) => approvals.complete(request.plan_id, receipt),
            Err(error) => {
                let code = match error {
                    DatasetImportError::Registration(ImportRegistrationError::InputChanged) => {
                        "INPUT_CHANGED"
                    }
                    DatasetImportError::Registration(ImportRegistrationError::BlockedInput) => {
                        "BLOCKED_INPUT"
                    }
                    _ => "IMPORT_FAILED",
                };
                if code == "INPUT_CHANGED" {
                    approvals.discard(request.plan_id);
                } else {
                    approvals.retry(request.plan_id, plan);
                }
                drop(approvals);
                return self.fail_confirmation(session, task_id, attempt_id, now_ms, code);
            }
        }
        drop(approvals);
        self.confirmation_task(session, task_id, now_ms)
    }

    fn fail_confirmation(
        &self,
        session: Uuid,
        task_id: Uuid,
        attempt_id: Uuid,
        now_ms: i64,
        code: &'static str,
    ) -> Result<ImportTaskSnapshot, DatasetImportError> {
        self.workspace
            .workspace_action(session, move |workspace| {
                workspace
                    .connection
                    .execute(
                        "UPDATE tasks SET status='FAILED', error_code=?5, finished_at_ms=?6,
                    updated_at_ms=?6, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3
                   AND current_attempt_id=?4 AND status='RUNNING'",
                        params![
                            task_id.to_string(),
                            OPERATION,
                            session.to_string(),
                            attempt_id.to_string(),
                            code,
                            now_ms
                        ],
                    )
                    .map_err(database)?;
                read_snapshot(&workspace.connection, task_id)
            })
            .map_err(DatasetImportError::Workspace)
    }
}

pub(super) fn find_request(
    connection: &Connection,
    operation: &str,
    key: Uuid,
    fingerprint: &[u8; 32],
) -> Result<Option<Uuid>, WorkspaceThreadError> {
    let result = connection.query_row(
        "SELECT task_id, request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
        params![operation, key.to_string()],
        |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
    ).optional().map_err(database)?;
    match result {
        Some((id, stored)) if stored == format_digest(fingerprint) => Ok(Some(id)),
        Some(_) => Err(WorkspaceThreadError::Task(
            TaskStoreError::IdempotencyConflict,
        )),
        None => Ok(None),
    }
}

fn read_snapshot(
    connection: &Connection,
    task_id: Uuid,
) -> Result<ImportTaskSnapshot, WorkspaceThreadError> {
    let (status, receipt, error_code, accepted_at_ms): (String, Option<String>, Option<String>, i64) = connection
        .query_row(
            "SELECT status, json_extract(execution_metadata_json, '$.import_receipt'), error_code, created_at_ms
         FROM tasks WHERE task_id=?1 AND operation_kind=?2",
            params![task_id.to_string(), OPERATION],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(database)?
        .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))?;
    let registration = receipt
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Receipt(error)))?;
    Ok(ImportTaskSnapshot {
        task_id,
        status,
        accepted_at_ms,
        registration,
        error_code,
    })
}

pub(super) fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
