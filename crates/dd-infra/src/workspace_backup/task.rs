//! Task admission and publication recovery for user-authorized Workspace backups.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    DATABASE, MANIFEST, MANIFEST_LIMIT, MARKER, Manifest, WorkspacePackageError,
    WorkspacePackageReceipt, create_workspace_backup_with, proof_file, read_manifest,
};
use crate::paths::publish_immutable;
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, format_digest};
use crate::workspace::OpenedWorkspace;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const KIND: &str = "WORKSPACE_BACKUP";
const CAPABILITY: &str = "workspace_backup.v1";
const RECOVERY_REQUIRED: &str = "BACKUP_RECOVERY_REQUIRED";

#[derive(Clone, Copy)]
pub struct BackupSubmission {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub destination_grant_id: Uuid,
    pub include_reports: bool,
}

pub struct BackupJob {
    task_id: Uuid,
    destination: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BackupReceipt {
    pub package: PathBuf,
    pub workspace_id: Uuid,
    pub asset_files: usize,
}

#[derive(Debug)]
pub struct BackupTaskSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub accepted_at_ms: i64,
    pub receipt: Option<BackupReceipt>,
}

#[derive(Deserialize, Serialize)]
struct Publication {
    receipt: BackupReceipt,
    manifest_sha256: String,
}

pub struct BackupService {
    owner: Arc<WorkspaceThread>,
}

impl BackupService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Self {
        Self { owner }
    }

    pub fn submit(
        &self,
        session: Uuid,
        submission: BackupSubmission,
        now_ms: i64,
    ) -> Result<(BackupTaskSnapshot, Option<BackupJob>), WorkspaceThreadError> {
        if !submission.include_reports {
            return Err(WorkspaceThreadError::Backup(
                WorkspacePackageError::ReportsRequired,
            ));
        }
        let _pause = self.owner.lock_final_operations()?;
        let key = submission.idempotency_key.to_string();
        let request_fingerprint: [u8; 32] = Sha256::digest(format!(
            "{CAPABILITY}:{}:with-reports",
            submission.destination_grant_id
        ))
        .into();
        let existing = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    "SELECT task_id, request_fingerprint FROM tasks
                 WHERE operation_kind=?1 AND idempotency_key=?2",
                    params![KIND, key],
                    |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(database)
        })?;
        if let Some((task, original)) = existing {
            if original != format_digest(&request_fingerprint) {
                return Err(WorkspaceThreadError::Task(
                    TaskStoreError::IdempotencyConflict,
                ));
            }
            let snapshot = self
                .snapshot(session, task, now_ms)?
                .ok_or_else(unknown_task)?;
            return Ok((snapshot, None));
        }

        self.recover_pending(session, now_ms)?;
        let parent = self
            .owner
            .consume_backup_directory(session, submission.destination_grant_id)?;
        let workspace = self.owner.storage_root(session)?;
        if parent.starts_with(&workspace) {
            return Err(WorkspaceThreadError::Backup(
                WorkspacePackageError::InvalidDestination,
            ));
        }
        let bytes = serde_json::to_vec(&json!({
            "contract_version": 1,
            "operation": KIND,
            "include_reports": true,
        }))
        .map_err(encode)?;
        let input_fingerprint: [u8; 32] = Sha256::digest(&bytes).into();
        let relative = Path::new("artifacts/task-inputs")
            .join(format!("{}.json", format_digest(&input_fingerprint)));
        publish_immutable(&workspace, &relative, &bytes)
            .map_err(WorkspacePackageError::Io)
            .map_err(WorkspaceThreadError::Backup)?;
        let stored = self.owner.enqueue_task(
            TaskSubmission {
                workspace_session: session,
                dataset_id: None,
                request_id: submission.request_id,
                idempotency_key: submission.idempotency_key,
                operation_kind: KIND.into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: relative,
                request_fingerprint,
                input_fingerprint,
            },
            now_ms,
        )?;
        let task_id = stored.task_id;
        Ok((
            BackupTaskSnapshot {
                task_id,
                status: stored.status,
                error_code: None,
                accepted_at_ms: stored.created_at_ms,
                receipt: None,
            },
            Some(BackupJob {
                task_id,
                destination: parent.join(format!("workspace-backup-{task_id}")),
            }),
        ))
    }

    pub fn run_admitted(
        &self,
        session: Uuid,
        job: BackupJob,
        now: impl Fn() -> i64 + Send + 'static,
    ) -> Result<(), WorkspaceThreadError> {
        let _pause = self.owner.lock_final_operations()?;
        self.owner.workspace_action(session, move |workspace| {
            let started = now();
            let task = job.task_id.to_string();
            let claimed = workspace.connection.execute(
                "UPDATE tasks SET status='RUNNING', started_at_ms=?4, updated_at_ms=?4,
                    row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'",
                params![task, KIND, session.to_string(), started],
            ).map_err(database)?;
            if claimed != 1 {
                return Err(unknown_task());
            }
            let destination = &job.destination;
            let result = if destination.parent().is_some_and(|parent| {
                parent.canonicalize().is_ok_and(|current| current == parent)
            }) {
                create_workspace_backup_with(workspace, destination, |stage, receipt| {
                    prepare(workspace, &task, destination, stage, receipt, now())
                })
            } else {
                Err(WorkspacePackageError::InvalidDestination)
            };
            let finished = now();
            let updated = match result {
                Ok(receipt) => finish(
                    workspace, &task, "SUCCEEDED", None,
                    Some(&backup_receipt(destination, &receipt)), finished,
                ),
                Err(error) => finish(
                    workspace, &task, "FAILED", Some(package_error_code(&error)), None, finished,
                ),
            };
            if updated.is_err() {
                let _ = workspace.connection.execute(
                    "UPDATE tasks SET status='INTERRUPTED', error_code=?3,
                        finished_at_ms=?4, updated_at_ms=?4, row_revision=row_revision+1
                     WHERE task_id=?1 AND operation_kind=?2 AND status='RUNNING'",
                    params![task, KIND, RECOVERY_REQUIRED, finished],
                );
            }
            updated
        })
    }

    pub fn snapshot(
        &self,
        session: Uuid,
        task: Uuid,
        now_ms: i64,
    ) -> Result<Option<BackupTaskSnapshot>, WorkspaceThreadError> {
        self.owner.workspace_action(session, move |workspace| {
            snapshot(workspace, session, task, now_ms)
        })
    }

    fn recover_pending(&self, session: Uuid, now_ms: i64) -> Result<(), WorkspaceThreadError> {
        self.owner.workspace_action(session, move |workspace| {
            let pending = {
                let mut statement = workspace.connection.prepare(
                    "SELECT task_id FROM file_operations WHERE operation_kind=?1 AND status='PREPARED'"
                ).map_err(database)?;
                statement.query_map([KIND], |row| row_uuid(row, 0)).map_err(database)?
                    .collect::<rusqlite::Result<Vec<_>>>().map_err(database)?
            };
            for task in pending {
                snapshot(workspace, session, task, now_ms)?;
            }
            Ok(())
        })
    }
}

fn snapshot(
    workspace: &mut OpenedWorkspace,
    session: Uuid,
    task_id: Uuid,
    now_ms: i64,
) -> Result<Option<BackupTaskSnapshot>, WorkspaceThreadError> {
    let task = task_id.to_string();
    let pending = workspace
        .connection
        .query_row(
            "SELECT f.manifest_path, f.manifest_digest
         FROM file_operations f JOIN tasks t ON f.task_id=t.task_id
         WHERE f.operation_id=?1 AND f.operation_kind=?2 AND f.status='PREPARED'
           AND ((t.workspace_session_id<>?3 AND t.status='RUNNING') OR
                (t.status='INTERRUPTED' AND t.error_code=?4))",
            params![task, KIND, session.to_string(), RECOVERY_REQUIRED],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(database)?;
    if let Some((path, digest)) = pending {
        match verify_publication(workspace, &task, &path, &digest) {
            Ok(receipt) => finish(workspace, &task, "SUCCEEDED", None, Some(&receipt), now_ms)?,
            Err(_) => finish(
                workspace,
                &task,
                "INTERRUPTED",
                Some("BACKUP_OUTPUT_UNVERIFIED"),
                None,
                now_ms,
            )?,
        }
    }
    workspace
        .connection
        .execute(
            "UPDATE tasks SET status='INTERRUPTED', error_code='SESSION_ENDED',
            finished_at_ms=?4, updated_at_ms=?4, row_revision=row_revision+1
         WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3
           AND status IN ('QUEUED','RUNNING')",
            params![task, KIND, session.to_string(), now_ms],
        )
        .map_err(database)?;
    let record = workspace
        .connection
        .query_row(
            "SELECT status, error_code, created_at_ms, execution_metadata_json
         FROM tasks WHERE task_id=?1 AND operation_kind=?2",
            params![task, KIND],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(database)?;
    record
        .map(|(status, error_code, accepted_at_ms, payload)| {
            let receipt = if status == "SUCCEEDED" {
                Some(serde_json::from_str(&payload).map_err(encode)?)
            } else {
                None
            };
            Ok(BackupTaskSnapshot {
                task_id,
                status,
                error_code,
                accepted_at_ms,
                receipt,
            })
        })
        .transpose()
}

fn prepare(
    workspace: &OpenedWorkspace,
    task: &str,
    destination: &Path,
    stage: &Path,
    receipt: &WorkspacePackageReceipt,
    now_ms: i64,
) -> Result<(), WorkspacePackageError> {
    let proof = proof_file(stage, Path::new(MANIFEST))?;
    let bytes = serde_json::to_vec(&Publication {
        receipt: backup_receipt(destination, receipt),
        manifest_sha256: proof.sha256,
    })
    .map_err(|_| WorkspacePackageError::InvalidManifest)?;
    let path = journal_path(task);
    publish_immutable(workspace.root(), Path::new(&path), &bytes)?;
    let changed = workspace.connection.execute(
        "INSERT INTO file_operations (
            operation_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
            operation_kind,task_id,status,manifest_path,manifest_digest,created_at_ms,updated_at_ms)
         SELECT task_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
            operation_kind,task_id,'PREPARED',?3,?4,?5,?5 FROM tasks
         WHERE task_id=?1 AND operation_kind=?2 AND status='RUNNING'",
        params![
            task,
            KIND,
            path,
            format_digest(&Sha256::digest(&bytes).into()),
            now_ms
        ],
    )?;
    if changed != 1 {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    Ok(())
}

fn finish(
    workspace: &mut OpenedWorkspace,
    task: &str,
    status: &str,
    error: Option<&str>,
    receipt: Option<&BackupReceipt>,
    now_ms: i64,
) -> Result<(), WorkspaceThreadError> {
    let payload = serde_json::to_string(&receipt).map_err(encode)?;
    let transaction = workspace.connection.transaction().map_err(database)?;
    let changed = transaction
        .execute(
            "UPDATE tasks SET status=?3, error_code=?4, execution_metadata_json=?5,
            finished_at_ms=?6, updated_at_ms=?6, row_revision=row_revision+1
         WHERE task_id=?1 AND operation_kind=?2
           AND (status='RUNNING' OR (status='INTERRUPTED' AND error_code=?7))",
            params![
                task,
                KIND,
                status,
                error,
                payload,
                now_ms,
                RECOVERY_REQUIRED
            ],
        )
        .map_err(database)?;
    if changed != 1 {
        return Err(unknown_task());
    }
    let operation_status = match status {
        "SUCCEEDED" => "COMMITTED",
        "INTERRUPTED" => "CONFLICT",
        _ => "FAILED",
    };
    transaction
        .execute(
            "UPDATE file_operations SET status=?3,error_code=?4,result_json=?5,
            completed_at_ms=?6,updated_at_ms=?6
         WHERE operation_id=?1 AND operation_kind=?2 AND status='PREPARED'",
            params![task, KIND, operation_status, error, payload, now_ms],
        )
        .map_err(database)?;
    transaction.commit().map_err(database)
}

fn verify_publication(
    workspace: &OpenedWorkspace,
    task: &str,
    path: &str,
    digest: &str,
) -> Result<BackupReceipt, WorkspacePackageError> {
    if path != journal_path(task) {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    let bytes = read_manifest(workspace.root(), Path::new(path), MANIFEST_LIMIT)?;
    if format_digest(&Sha256::digest(&bytes).into()) != digest {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    let publication: Publication =
        serde_json::from_slice(&bytes).map_err(|_| WorkspacePackageError::InvalidManifest)?;
    let receipt = publication.receipt;
    let root = &receipt.package;
    if root.canonicalize()? != *root
        || root.starts_with(workspace.root())
        || root.file_name().and_then(|name| name.to_str())
            != Some(format!("workspace-backup-{task}").as_str())
        || receipt.workspace_id.to_string() != workspace.identity().workspace_id
    {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    let bytes = read_manifest(root, Path::new(MANIFEST), MANIFEST_LIMIT)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| WorkspacePackageError::InvalidManifest)?;
    if format_digest(&Sha256::digest(&bytes).into()) != publication.manifest_sha256
        || manifest.version != 1
        || manifest.workspace_id != receipt.workspace_id
        || manifest.assets.len() != receipt.asset_files
    {
        return Err(WorkspacePackageError::InvalidManifest);
    }
    for (name, proof) in [(MARKER, manifest.marker), (DATABASE, manifest.database)] {
        if proof_file(root, Path::new(name))? != proof {
            return Err(WorkspacePackageError::DigestMismatch(name.into()));
        }
    }
    let files = manifest
        .assets
        .iter()
        .map(|file| (&file.package_path, &file.sha256, file.size_bytes))
        .chain(
            manifest
                .artifacts
                .iter()
                .map(|file| (&file.package_path, &file.sha256, file.size_bytes)),
        );
    for (path, digest, size) in files {
        let proof = proof_file(root, Path::new(path))?;
        if proof.sha256 != *digest || proof.size_bytes != size {
            return Err(WorkspacePackageError::DigestMismatch(path.into()));
        }
    }
    Ok(receipt)
}

fn journal_path(task: &str) -> String {
    format!("artifacts/backup-recovery/{task}.json")
}

fn backup_receipt(destination: &Path, receipt: &WorkspacePackageReceipt) -> BackupReceipt {
    BackupReceipt {
        package: destination.to_owned(),
        workspace_id: receipt.workspace_id,
        asset_files: receipt.asset_files,
    }
}

fn unknown_task() -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::UnknownTask)
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}

fn encode(error: serde_json::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Receipt(error))
}

fn package_error_code(error: &WorkspacePackageError) -> &'static str {
    match error {
        WorkspacePackageError::DestinationExists => "DESTINATION_EXISTS",
        WorkspacePackageError::InvalidDestination => "DESTINATION_UNAVAILABLE",
        WorkspacePackageError::PendingMutation => "WORKSPACE_RECOVERY_REQUIRED",
        WorkspacePackageError::DigestMismatch(_)
        | WorkspacePackageError::Assets(_)
        | WorkspacePackageError::Artifacts(_) => "BACKUP_INPUT_UNVERIFIED",
        _ => "BACKUP_FAILED",
    }
}
