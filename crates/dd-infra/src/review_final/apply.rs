use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;
use tempfile::NamedTempFile;
use uuid::Uuid;

use super::{
    FinalManifest, FinalOperation, FinalOperationState, KIND, MAX_ANNOTATION, MAX_IMAGE,
    ReviewFinalError, ReviewFinalService, database, digest, file_digest, new_annotation_path,
    operation_state, verified_parent,
};
use crate::json::strict_json;
use crate::paths::open_scoped_file;
use crate::review_store::ReviewStatus;
use crate::workspace_thread::WorkspaceThreadError;

struct PendingOperation {
    root: PathBuf,
    status: String,
    path: String,
    sha256: String,
    review: String,
    dataset: String,
    asset: String,
}

impl ReviewFinalService {
    /// Complete an accepted operation, or resume it after a Workspace reopen.
    /// Actual file bytes determine whether replacement still needs to happen.
    pub fn resume(
        &self,
        session: Uuid,
        operation: Uuid,
        now_ms: i64,
    ) -> Result<FinalOperation, ReviewFinalError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(ReviewFinalError::Storage)?;
        let pending = self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT status, manifest_path, manifest_digest, review_id, dataset_id, asset_id
                 FROM file_operations WHERE operation_id=?1 AND operation_kind=?2",
                params![operation.to_string(), KIND],
                |row| Ok(PendingOperation {
                    root: owner.root().to_owned(), status: row.get(0)?, path: row.get(1)?,
                    sha256: row.get(2)?, review: row.get(3)?, dataset: row.get(4)?, asset: row.get(5)?,
                }),
            ).optional().map_err(database)
        }).map_err(ReviewFinalError::Storage)?.ok_or(ReviewFinalError::InvalidOperation)?;
        let receipt = operation_state(&operation.to_string(), &pending.status)?;
        if receipt.state != FinalOperationState::Prepared {
            return Ok(receipt);
        }
        self.owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "UPDATE tasks SET status='RUNNING', error_code=NULL, finished_at_ms=NULL,
                     started_at_ms=?3, updated_at_ms=?3, row_revision=row_revision+1
                 WHERE task_id=(SELECT task_id FROM file_operations WHERE operation_id=?1 AND operation_kind=?2)
                 AND status IN ('QUEUED','INTERRUPTED')",
                params![operation.to_string(), KIND, now_ms],
            ).map(|_| ()).map_err(database)
        }).map_err(ReviewFinalError::Storage)?;
        let prefix = format!("artifacts/file-operations/{operation}");
        if pending.path != format!("{prefix}/operation.json") {
            return Err(ReviewFinalError::InvalidOperation);
        }
        let envelope = pending.read_verified(&pending.path, &pending.sha256, 64 * 1024)?;
        let document = strict_json(&envelope).map_err(|_| ReviewFinalError::InvalidOperation)?;
        let manifest: FinalManifest =
            serde_json::from_value(document).map_err(ReviewFinalError::Json)?;
        if manifest.contract_version != 1
            || manifest.operation_id != operation.to_string()
            || manifest.review_id != pending.review
            || manifest.dataset_id != pending.dataset
            || manifest.asset_id != pending.asset
            || manifest.payload_path != format!("{prefix}/annotation.json")
        {
            return Err(ReviewFinalError::InvalidOperation);
        }
        let payload = pending.read_verified(
            &manifest.payload_path,
            &manifest.new_annotation_digest,
            MAX_ANNOTATION,
        )?;
        let binding = self.current_binding(session, &manifest)?;
        let state = if binding {
            apply_files(&manifest, &payload)?
        } else {
            FinalOperationState::Conflict
        };
        self.record_resolution(session, operation, manifest, state, now_ms)?;
        Ok(FinalOperation {
            operation_id: operation,
            state,
        })
    }

    fn current_binding(
        &self,
        session: Uuid,
        manifest: &FinalManifest,
    ) -> Result<bool, ReviewFinalError> {
        let review = stored_uuid(&manifest.review_id)?;
        let dataset = stored_uuid(&manifest.dataset_id)?;
        let asset = stored_uuid(&manifest.asset_id)?;
        let (binding, location, review_matches, asset_matches) = self
            .owner
            .workspace_action(session, move |owner| {
                let review = owner
                    .get_review(review)
                    .map_err(WorkspaceThreadError::Review)?;
                let binding = owner
                    .registered_image(dataset, asset)
                    .map_err(WorkspaceThreadError::Dataset)?;
                let (location, revision): (String, i64) = owner
                    .connection
                    .query_row(
                        "SELECT location_kind, row_revision FROM assets WHERE asset_id=?1",
                        [asset.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(database)?;
                Ok((binding, location, review, revision))
            })
            .map_err(ReviewFinalError::Storage)?;
        if review_matches.status != ReviewStatus::Committing
            || i64::try_from(review_matches.row_revision).ok() != Some(manifest.review_revision)
            || asset_matches != manifest.asset_revision
            || binding.image.path != manifest.image_path
            || binding.image.sha256 != manifest.old_image_digest
            || binding.label_schema_fingerprint != manifest.label_schema_digest
            || binding.annotation.as_ref().map(|file| &file.sha256)
                != manifest.old_annotation_digest.as_ref()
        {
            return Ok(false);
        }
        let target = if let Some(annotation) = binding.annotation {
            annotation.path
        } else {
            new_annotation_path(&binding.image.path)?
        };
        if target != manifest.target_path {
            return Ok(false);
        }
        let registry = path_string(&manifest.registry_path)?.to_owned();
        if match location.as_str() {
            "EXTERNAL" => manifest.registry_path != target,
            "WORKSPACE" => {
                let root = self
                    .owner
                    .storage_root(session)
                    .map_err(ReviewFinalError::Storage)?;
                target.strip_prefix(root).ok() != Some(manifest.registry_path.as_path())
            }
            _ => true,
        } {
            return Ok(false);
        }
        let available = self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT NOT EXISTS (SELECT 1 FROM assets WHERE location_kind=?1 AND annotation_path=?2 AND asset_id<>?3)",
                params![location, registry, asset.to_string()], |row| row.get(0),
            ).map_err(database)
        }).map_err(ReviewFinalError::Storage)?;
        Ok(available)
    }

    fn record_resolution(
        &self,
        session: Uuid,
        operation: Uuid,
        manifest: FinalManifest,
        state: FinalOperationState,
        now_ms: i64,
    ) -> Result<(), ReviewFinalError> {
        let annotation_path = path_string(&manifest.registry_path)?.to_owned();
        self.owner.workspace_action(session, move |owner| {
            let transaction = owner.connection.transaction().map_err(database)?;
            let (task_status, error_code) = if state == FinalOperationState::Committed {
                commit_rows(&transaction, &manifest, &annotation_path, now_ms)?;
                let result = json!({
                    "operation_id": operation.to_string(),
                    "review_id": manifest.review_id,
                    "annotation_digest": manifest.new_annotation_digest,
                    "confirmed_negative": manifest.confirmed_negative
                });
                expect_one(transaction.execute(
                    "UPDATE file_operations SET status='COMMITTED', result_json=?1, completed_at_ms=?2, updated_at_ms=?2
                     WHERE operation_id=?3 AND status='PREPARED'",
                    params![result.to_string(), now_ms, operation.to_string()],
                ).map_err(database)?)?;
                ("SUCCEEDED", None)
            } else {
                expect_one(transaction.execute(
                    "UPDATE review_sessions SET status='CONFLICT', row_revision=row_revision+1, updated_at_ms=?1
                     WHERE review_id=?2 AND row_revision=?3 AND status='COMMITTING'",
                    params![now_ms, manifest.review_id, manifest.review_revision],
                ).map_err(database)?)?;
                expect_one(transaction.execute(
                    "UPDATE file_operations SET status='CONFLICT', error_code='INPUT_CHANGED', completed_at_ms=?1, updated_at_ms=?1
                     WHERE operation_id=?2 AND status='PREPARED'",
                    params![now_ms, operation.to_string()],
                ).map_err(database)?)?;
                ("FAILED", Some("INPUT_CHANGED"))
            };
            transaction.execute(
                "UPDATE tasks SET status=?2, error_code=?3, finished_at_ms=?4, updated_at_ms=?4,
                     row_revision=row_revision+1
                 WHERE task_id=(SELECT task_id FROM file_operations WHERE operation_id=?1)",
                params![operation.to_string(), task_status, error_code, now_ms],
            ).map_err(database)?;
            transaction.commit().map_err(database)
        }).map_err(ReviewFinalError::Storage)
    }
}

impl PendingOperation {
    fn read_verified(
        &self,
        path: &str,
        sha: &str,
        limit: u64,
    ) -> Result<Vec<u8>, ReviewFinalError> {
        let mut bytes = Vec::new();
        open_scoped_file(&self.root, Path::new(path))?
            .take(limit + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit || digest(&bytes) != sha {
            return Err(ReviewFinalError::InvalidOperation);
        }
        Ok(bytes)
    }
}

fn apply_files(
    manifest: &FinalManifest,
    payload: &[u8],
) -> Result<FinalOperationState, ReviewFinalError> {
    if file_digest(&manifest.image_path, MAX_IMAGE)?.as_deref()
        != Some(manifest.old_image_digest.as_str())
    {
        return Ok(FinalOperationState::Conflict);
    }
    let target = &manifest.target_path;
    let current = file_digest(target, MAX_ANNOTATION)?;
    if current.as_deref() == Some(manifest.new_annotation_digest.as_str()) {
        return Ok(FinalOperationState::Committed);
    }
    if current != manifest.old_annotation_digest {
        return Ok(FinalOperationState::Conflict);
    }
    let mut temporary = NamedTempFile::new_in(verified_parent(target)?)?;
    temporary.write_all(payload)?;
    temporary.as_file().sync_all()?;
    if file_digest(target, MAX_ANNOTATION)? != current {
        return Ok(FinalOperationState::Conflict);
    }
    let published = if current.is_some() {
        temporary.persist(target)
    } else {
        temporary.persist_noclobber(target)
    };
    match published {
        Ok(file) => drop(file),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(FinalOperationState::Conflict);
        }
        Err(error) => return Err(error.error.into()),
    }
    if file_digest(target, MAX_ANNOTATION)?.as_deref()
        != Some(manifest.new_annotation_digest.as_str())
    {
        return Ok(FinalOperationState::Conflict);
    }
    Ok(FinalOperationState::Committed)
}

fn commit_rows(
    transaction: &Transaction<'_>,
    manifest: &FinalManifest,
    registry_path: &str,
    now_ms: i64,
) -> Result<(), WorkspaceThreadError> {
    expect_one(transaction.execute(
        "UPDATE assets SET annotation_path=?1, annotation_digest=?2, annotation_format='COCO_IMAGE_BBOX',
             annotation_origin=CASE WHEN annotation_origin='NONE' THEN 'APP_CREATED' ELSE annotation_origin END,
             annotation_local_image_id=?3, row_revision=row_revision+1, updated_at_ms=?4
         WHERE asset_id=?5 AND row_revision=?6 AND media_digest=?7 AND annotation_digest IS ?8",
        params![registry_path, manifest.new_annotation_digest, manifest.local_image_id, now_ms,
            manifest.asset_id, manifest.asset_revision, manifest.old_image_digest, manifest.old_annotation_digest],
    ).map_err(database)?)?;
    transaction.execute(
        "UPDATE review_sessions SET status='STALE', row_revision=row_revision+1, updated_at_ms=?1
         WHERE review_id=COALESCE(
             (SELECT final_review_id FROM dataset_assets WHERE dataset_id=?2 AND asset_id=?3),
             (SELECT review_id FROM staging_items WHERE target_dataset_id=?2 AND asset_id=?3)
         )
           AND status='CONFIRMED' AND (confirmed_media_digest IS NOT ?4
             OR confirmed_annotation_digest IS NOT ?5 OR confirmed_label_schema_digest IS NOT ?6)",
        params![now_ms, manifest.dataset_id, manifest.asset_id, manifest.old_image_digest,
            manifest.new_annotation_digest, manifest.label_schema_digest],
    ).map_err(database)?;
    expect_one(transaction.execute(
        "UPDATE review_sessions SET status='CONFIRMED', review_manifest_path=NULL, row_revision=row_revision+1,
             confirmed_media_digest=?1, confirmed_annotation_digest=?2, confirmed_label_schema_digest=?3,
             confirmed_at_ms=?4, updated_at_ms=?4
         WHERE review_id=?5 AND status='COMMITTING' AND row_revision=?6",
        params![manifest.old_image_digest, manifest.new_annotation_digest, manifest.label_schema_digest,
            now_ms, manifest.review_id, manifest.review_revision],
    ).map_err(database)?)?;
    let member = transaction
        .execute(
            "UPDATE dataset_assets SET final_review_id=?1 WHERE dataset_id=?2 AND asset_id=?3",
            params![manifest.review_id, manifest.dataset_id, manifest.asset_id],
        )
        .map_err(database)?;
    let staged = transaction
        .execute(
            "UPDATE staging_items SET review_id=?1, check_task_id=NULL,
             check_acceptance_digest=NULL, check_accepted_at_ms=NULL
         WHERE target_dataset_id=?2 AND asset_id=?3",
            params![manifest.review_id, manifest.dataset_id, manifest.asset_id],
        )
        .map_err(database)?;
    expect_one(member + staged)
}

fn expect_one(changed: usize) -> Result<(), WorkspaceThreadError> {
    if changed != 1 {
        return Err(WorkspaceThreadError::Review(
            crate::review_store::ReviewStoreError::RevisionConflict,
        ));
    }
    Ok(())
}

fn stored_uuid(text: &str) -> Result<Uuid, ReviewFinalError> {
    Uuid::parse_str(text).map_err(|_| ReviewFinalError::InvalidOperation)
}

fn path_string(path: &Path) -> Result<&str, ReviewFinalError> {
    path.to_str().ok_or(ReviewFinalError::InvalidOperation)
}
