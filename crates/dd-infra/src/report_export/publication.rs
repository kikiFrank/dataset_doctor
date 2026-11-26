//! Journal a single-file publication so its receipt can be recovered after exit.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{Builder, NamedTempFile};
use uuid::Uuid;

use super::{
    CAPABILITY, OPERATION, ReportExportError, ReportExportReceipt, database, evidence, file_name,
};
use crate::json::strict_json;
use crate::paths::{open_scoped_file, publish_immutable};
use crate::task_store::format_digest;
use crate::worker_runtime::now_ms;
use crate::workspace_thread::WorkspaceThread;

const JOURNAL_LIMIT: u64 = 32 * 1024;
const OUTPUT_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Publication {
    pub(super) destination: PathBuf,
    pub(super) receipt: ReportExportReceipt,
}

impl Publication {
    pub(super) fn stage(&self, bytes: &[u8]) -> Result<NamedTempFile, ReportExportError> {
        check_destination(&self.destination)?;
        if bytes.len() as u64 > OUTPUT_LIMIT {
            return Err(ReportExportError::InvalidReport);
        }
        let mut staged = Builder::new()
            .prefix(".dd-report-")
            .tempfile_in(&self.destination)?;
        staged.write_all(bytes)?;
        staged.as_file().sync_all()?;
        Ok(staged)
    }

    pub(super) fn prepare(
        &self,
        owner: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
    ) -> Result<(), ReportExportError> {
        let path = journal_path(task);
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > JOURNAL_LIMIT {
            return Err(ReportExportError::InvalidTask);
        }
        let root = owner.storage_root(session)?;
        publish_immutable(&root, Path::new(&path), &bytes)?;
        let hash = format_digest(&Sha256::digest(&bytes).into());
        let now = now_ms().map_err(evidence)?;
        let inserted = owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "INSERT INTO file_operations(
                    operation_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
                    operation_kind,dataset_id,task_id,status,manifest_path,manifest_digest,created_at_ms,updated_at_ms)
                 SELECT task_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
                    operation_kind,dataset_id,task_id,'PREPARED',?4,?5,?6,?6
                 FROM tasks WHERE task_id=?1 AND operation_kind=?2 AND capability_id=?3 AND status='RUNNING'",
                params![task.to_string(), OPERATION, CAPABILITY, path, hash, now],
            ).map_err(database)
        })?;
        if inserted != 1 {
            return Err(ReportExportError::InvalidTask);
        }
        Ok(())
    }

    fn verified_receipt(self, task: Uuid) -> Result<ReportExportReceipt, ReportExportError> {
        if self.receipt.file_name != file_name(task, self.receipt.format) {
            return Err(ReportExportError::InvalidTask);
        }
        check_destination(&self.destination)?;
        let size: u64 = self.receipt.size_bytes.parse().map_err(evidence)?;
        if size > OUTPUT_LIMIT {
            return Err(ReportExportError::InvalidTask);
        }
        let mut bytes = Vec::new();
        open_scoped_file(&self.destination, Path::new(&self.receipt.file_name))?
            .take(size + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size
            || format_digest(&Sha256::digest(&bytes).into()) != self.receipt.sha256
        {
            return Err(ReportExportError::InvalidTask);
        }
        Ok(self.receipt)
    }
}

pub(super) fn write_export(
    destination: &Path,
    filename: &str,
    staged: NamedTempFile,
) -> Result<(), ReportExportError> {
    check_destination(destination)?;
    staged
        .persist_noclobber(destination.join(filename))
        .map_err(|error| error.error)?;
    Ok(())
}

pub(super) fn recover_publication(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
) -> Result<(), ReportExportError> {
    let pending = owner.workspace_action(session, move |owner| {
        owner.connection.query_row(
            "SELECT f.manifest_path,f.manifest_digest FROM file_operations f JOIN tasks t ON f.task_id=t.task_id
             WHERE f.operation_id=?1 AND f.operation_kind=?2 AND f.status='PREPARED'
                AND t.capability_id=?3 AND t.status='RUNNING'",
            params![task.to_string(), OPERATION, CAPABILITY],
            |row| Ok((owner.root().to_owned(), row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional().map_err(database)
    })?;
    let Some((root, path, hash)) = pending else {
        return Ok(());
    };
    match read_journal(&root, task, &path, &hash).and_then(|item| item.verified_receipt(task)) {
        Ok(receipt) => finish(owner, session, task, "SUCCEEDED", None, Some(receipt)),
        Err(_) => finish(
            owner,
            session,
            task,
            "INTERRUPTED",
            Some("REPORT_OUTPUT_UNVERIFIED"),
            None,
        ),
    }
}

pub(super) fn finish(
    owner: &WorkspaceThread,
    session: Uuid,
    task: Uuid,
    status: &'static str,
    error: Option<&'static str>,
    receipt: Option<ReportExportReceipt>,
) -> Result<(), ReportExportError> {
    let now = now_ms().map_err(evidence)?;
    let result = serde_json::to_string(&receipt)?;
    let operation_status = match status {
        "SUCCEEDED" => "COMMITTED",
        "INTERRUPTED" => "CONFLICT",
        _ => "FAILED",
    };
    let updated = owner.workspace_action(session, move |owner| {
        let transaction = owner.connection.transaction().map_err(database)?;
        let changed = transaction.execute(
            "UPDATE tasks SET status=?4,error_code=?5,execution_metadata_json=?6,finished_at_ms=?7,updated_at_ms=?7,
                row_revision=row_revision+1 WHERE task_id=?1 AND operation_kind=?2 AND capability_id=?3 AND status='RUNNING'",
            params![task.to_string(), OPERATION, CAPABILITY, status, error, result, now],
        ).map_err(database)?;
        if changed != 1 {
            return Ok(false);
        }
        let prepared = transaction.execute(
            "UPDATE file_operations SET status=?3,error_code=?4,result_json=?5,completed_at_ms=?6,updated_at_ms=?6
             WHERE operation_id=?1 AND operation_kind=?2 AND status='PREPARED'",
            params![task.to_string(), OPERATION, operation_status, error, result, now],
        ).map_err(database)?;
        if status == "SUCCEEDED" && prepared != 1 {
            return Ok(false);
        }
        transaction.commit().map_err(database)?;
        Ok(true)
    })?;
    if !updated {
        return Err(ReportExportError::InvalidTask);
    }
    Ok(())
}

fn read_journal(
    root: &Path,
    task: Uuid,
    path: &str,
    hash: &str,
) -> Result<Publication, ReportExportError> {
    if path != journal_path(task) {
        return Err(ReportExportError::InvalidTask);
    }
    let mut bytes = Vec::new();
    open_scoped_file(root, Path::new(path))?
        .take(JOURNAL_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > JOURNAL_LIMIT || format_digest(&Sha256::digest(&bytes).into()) != hash {
        return Err(ReportExportError::InvalidTask);
    }
    serde_json::from_value(strict_json(&bytes).map_err(evidence)?).map_err(Into::into)
}

fn journal_path(task: Uuid) -> String {
    format!("artifacts/task-publications/report-{task}.json")
}

fn check_destination(path: &Path) -> io::Result<()> {
    if !path.is_dir() || path.canonicalize()? != path {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Report export destination moved",
        ));
    }
    Ok(())
}
