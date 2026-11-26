use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::task::{KIND, database};
use super::{CocoExportError, CocoExportReceipt, CocoExportService, IMAGE_LIMIT};
use crate::paths::{open_scoped_file, publish_immutable};
use crate::workspace_thread::WorkspaceThreadError;

const JOURNAL_LIMIT: u64 = 16 * 1024 * 1024;
const INDEX_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct Publication<T> {
    receipt: T,
    annotations_sha256: String,
    index_sha256: String,
}

#[derive(Deserialize)]
struct ExportIndex {
    images: Vec<ExportImage>,
}

#[derive(Deserialize)]
struct ExportImage {
    path: PathBuf,
    image_digest: String,
}

impl CocoExportService {
    pub(super) fn prepare_publication(
        &self,
        session: Uuid,
        task: Uuid,
        stage: &Path,
        receipt: &CocoExportReceipt,
        now_ms: i64,
    ) -> Result<(), CocoExportError> {
        let stage = stage.canonicalize()?;
        let bytes = serde_json::to_vec(&Publication {
            receipt,
            annotations_sha256: scoped_digest(&stage, Path::new("annotations.json"), INDEX_LIMIT)?,
            index_sha256: scoped_digest(&stage, Path::new("manifest.json"), INDEX_LIMIT)?,
        })
        .map_err(CocoExportError::Json)?;
        let path = journal_path(task);
        let root = self
            .owner
            .storage_root(session)
            .map_err(CocoExportError::Storage)?;
        publish_immutable(&root, Path::new(&path), &bytes)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let changed = self.owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "INSERT INTO file_operations (
                    operation_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
                    operation_kind,dataset_id,task_id,status,manifest_path,manifest_digest,created_at_ms,updated_at_ms)
                 SELECT task_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
                    operation_kind,dataset_id,task_id,'PREPARED',?3,?4,?5,?5 FROM tasks
                 WHERE task_id=?1 AND operation_kind=?2 AND status='RUNNING'",
                params![task.to_string(), KIND, path, digest, now_ms],
            ).map_err(database)
        }).map_err(CocoExportError::Storage)?;
        if changed != 1 {
            return Err(CocoExportError::InvalidTask);
        }
        Ok(())
    }

    pub(super) fn recover_publication(
        &self,
        session: Uuid,
        task: Uuid,
        now_ms: i64,
    ) -> Result<(), CocoExportError> {
        let pending = self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT f.manifest_path,f.manifest_digest FROM file_operations f JOIN tasks t ON f.task_id=t.task_id
                 WHERE f.operation_id=?1 AND f.operation_kind=?2 AND f.status='PREPARED'
                  AND ((t.workspace_session_id<>?3 AND t.status='RUNNING') OR
                       (t.status='INTERRUPTED' AND t.error_code='EXPORT_RECOVERY_REQUIRED'))",
                params![task.to_string(), KIND, session.to_string()],
                |row| Ok((owner.root().to_owned(), row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).optional().map_err(database)
        }).map_err(CocoExportError::Storage)?;
        let Some((root, path, digest)) = pending else {
            return Ok(());
        };
        let (status, error, payload) = match verify_publication(task, &root, &path, &digest) {
            Ok(receipt) => (
                "SUCCEEDED",
                None,
                serde_json::to_string(&receipt).map_err(CocoExportError::Json)?,
            ),
            Err(_) => (
                "INTERRUPTED",
                Some("EXPORT_OUTPUT_UNVERIFIED"),
                "{}".to_owned(),
            ),
        };
        self.owner
            .workspace_action(session, move |owner| {
                let transaction = owner.connection.transaction().map_err(database)?;
                let updated = transaction
                    .execute(
                        "UPDATE tasks SET status=?3,error_code=?4,execution_metadata_json=?5,
                    finished_at_ms=?6,updated_at_ms=?6,row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2
                   AND ((workspace_session_id<>?7 AND status='RUNNING') OR
                        (status='INTERRUPTED' AND error_code='EXPORT_RECOVERY_REQUIRED'))",
                        params![
                            task.to_string(),
                            KIND,
                            status,
                            error,
                            payload,
                            now_ms,
                            session.to_string()
                        ],
                    )
                    .map_err(database)?;
                if updated == 1 {
                    finish_publication(&transaction, task, status, error, &payload, now_ms)?;
                }
                transaction.commit().map_err(database)
            })
            .map_err(CocoExportError::Storage)
    }
}

pub(super) fn finish_publication(
    transaction: &Transaction<'_>,
    task: Uuid,
    task_status: &str,
    error_code: Option<&str>,
    payload: &str,
    now_ms: i64,
) -> Result<(), WorkspaceThreadError> {
    let status = match task_status {
        "SUCCEEDED" => "COMMITTED",
        "INTERRUPTED" => "CONFLICT",
        _ => "FAILED",
    };
    transaction
        .execute(
            "UPDATE file_operations SET status=?3,error_code=?4,result_json=?5,
            completed_at_ms=?6,updated_at_ms=?6
         WHERE operation_id=?1 AND operation_kind=?2 AND status='PREPARED'",
            params![task.to_string(), KIND, status, error_code, payload, now_ms],
        )
        .map(|_| ())
        .map_err(database)
}

fn verify_publication(
    task: Uuid,
    workspace_root: &Path,
    path: &str,
    digest: &str,
) -> Result<CocoExportReceipt, CocoExportError> {
    if path != journal_path(task) {
        return Err(CocoExportError::InvalidTask);
    }
    let journal_bytes = bounded_bytes(workspace_root, Path::new(path), JOURNAL_LIMIT)?;
    if format!("{:x}", Sha256::digest(&journal_bytes)) != digest {
        return Err(CocoExportError::InvalidTask);
    }
    let journal: Publication<CocoExportReceipt> =
        serde_json::from_slice(&journal_bytes).map_err(CocoExportError::Json)?;
    let package = &journal.receipt.package;
    let expected_name = format!("dataset-{task}");
    if !package.is_absolute()
        || package.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
        || !package.is_dir()
        || package.canonicalize()? != *package
    {
        return Err(CocoExportError::InputChanged);
    }
    if scoped_digest(package, Path::new("annotations.json"), INDEX_LIMIT)?
        != journal.annotations_sha256
    {
        return Err(CocoExportError::InputChanged);
    }
    let index = bounded_bytes(package, Path::new("manifest.json"), INDEX_LIMIT)?;
    if format!("{:x}", Sha256::digest(&index)) != journal.index_sha256 {
        return Err(CocoExportError::InputChanged);
    }
    let index: ExportIndex = serde_json::from_slice(&index).map_err(CocoExportError::Json)?;
    if index.images.len() != journal.receipt.image_count {
        return Err(CocoExportError::InvalidData);
    }
    for image in index.images {
        if !image.path.starts_with("images")
            || scoped_digest(package, &image.path, IMAGE_LIMIT)? != image.image_digest
        {
            return Err(CocoExportError::InputChanged);
        }
    }
    Ok(journal.receipt)
}

fn scoped_digest(root: &Path, path: &Path, limit: u64) -> Result<String, CocoExportError> {
    let mut hash = Sha256::new();
    if io::copy(
        &mut open_scoped_file(root, path)?.take(limit + 1),
        &mut hash,
    )? > limit
    {
        return Err(CocoExportError::InvalidData);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn bounded_bytes(root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>, CocoExportError> {
    let mut bytes = Vec::new();
    open_scoped_file(root, path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(CocoExportError::InvalidData);
    }
    Ok(bytes)
}

fn journal_path(task: Uuid) -> String {
    format!("artifacts/exports/{task}/publication.json")
}
