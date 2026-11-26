//! Explicit and recoverable application of a verified color candidate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::color_preview::{
    ColorOptions, ColorPreviewService, ColorRegion, MAX_IMAGE_BYTES, read_scoped,
};
use crate::dataset_query::RegisteredImage;
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::strict_json;
use crate::paths::{publish_immutable, registered_digest, replace_verified_file};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub use crate::worker_runtime::WorkerRuntimeError as ColorApplyError;

pub(crate) const KIND: &str = "COLOR_APPLY";
const CAPABILITY: &str = "color_apply.v1";
const MANIFEST_LIMIT: u64 = 128 * 1024;
const ANNOTATION_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ColorApplySubmission {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub candidate_ref: ArtifactReference,
    pub base_image_digest: String,
    pub expected_asset_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorApplyState {
    Prepared,
    Committed,
    Conflict,
}

#[derive(Clone, Debug)]
pub struct ColorApplyOperation {
    pub operation_id: Uuid,
    pub task_id: Uuid,
    pub state: ColorApplyState,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApplyManifest {
    contract_version: u8,
    operation_id: Uuid,
    dataset_id: Uuid,
    asset_id: Uuid,
    asset_revision: i64,
    image_path: PathBuf,
    base_image_digest: String,
    base_annotation_digest: Option<String>,
    label_schema_digest: String,
    candidate_ref: ArtifactReference,
    payload_path: String,
    region: ColorRegion,
    options: ColorOptions,
    changed_pixels: u64,
    lossy_reencode: bool,
}

struct ApplyTarget {
    root: PathBuf,
    image: RegisteredImage,
    revision: i64,
    pending: bool,
}

struct StoredOperation {
    id: Uuid,
    task: Uuid,
    status: String,
    dataset: Uuid,
    asset: Uuid,
    path: String,
    sha256: String,
    fingerprint: String,
}

const SELECT_OPERATION: &str =
    "SELECT operation_id,task_id,status,dataset_id,asset_id,manifest_path,manifest_digest,request_fingerprint
     FROM file_operations WHERE operation_kind=?1";

pub struct ColorApplyService {
    owner: Arc<WorkspaceThread>,
    previews: ColorPreviewService,
}

impl ColorApplyService {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, ColorApplyError> {
        Ok(Self {
            previews: ColorPreviewService::new(Arc::clone(&owner))?,
            owner,
        })
    }

    pub fn prepare(
        &self,
        session: Uuid,
        request: ColorApplySubmission,
        now: i64,
    ) -> Result<ColorApplyOperation, ColorApplyError> {
        let invalid = || ColorApplyError::InvalidBinding;
        if request.request_id.is_nil() || request.idempotency_key.is_nil() {
            return Err(invalid());
        }
        let ArtifactOwner::Task(preview_task) = request.candidate_ref.owner else {
            return Err(invalid());
        };
        let _guard = self.owner.lock_final_operations()?;
        let dataset = request.dataset_id;
        let asset = request.asset_id;
        let fingerprint: [u8; 32] = Sha256::digest(
            json!([
                CAPABILITY,
                dataset,
                asset,
                request.base_image_digest,
                request.expected_asset_revision.to_string(),
                &request.candidate_ref,
            ])
            .to_string()
            .as_bytes(),
        )
        .into();
        let key = request.idempotency_key.to_string();
        if let Some(previous) = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    &format!("{SELECT_OPERATION} AND idempotency_key=?2"),
                    params![KIND, key],
                    StoredOperation::read,
                )
                .optional()
                .map_err(database)
        })? {
            if previous.fingerprint != format_digest(&fingerprint) {
                return Err(WorkspaceThreadError::Task(TaskStoreError::IdempotencyConflict).into());
            }
            return previous.receipt();
        }
        let preview = self
            .previews
            .snapshot(session, preview_task)?
            .preview
            .ok_or_else(invalid)?;
        if preview.dataset_id != dataset
            || preview.asset_id != asset
            || preview.base_image_digest != request.base_image_digest
            || preview.asset_revision != request.expected_asset_revision
            || preview.candidate_ref != request.candidate_ref
            || preview.changed_pixels == 0
        {
            return Err(invalid());
        }
        let target = self.target(session, dataset, asset)?;
        let revision = i64::try_from(request.expected_asset_revision).map_err(|_| invalid())?;
        if target.pending
            || target.revision != revision
            || target.image.image.sha256 != preview.base_image_digest
            || target.image.annotation.as_ref().map(|file| &file.sha256)
                != preview.base_annotation_digest.as_ref()
            || target.image.label_schema_fingerprint != preview.label_schema_digest
            || !target.image.image.live_digest(MAX_IMAGE_BYTES)?
            || !current_annotation(&target.image)?
        {
            return Err(invalid());
        }
        let bytes = read_scoped(&target.root, &preview.candidate_path, MAX_IMAGE_BYTES)?;
        if digest(&bytes) != preview.candidate_ref.sha256 {
            return Err(ColorApplyError::ResultUnconfirmed);
        }

        let operation_id = Uuid::new_v4();
        let directory = format!("artifacts/file-operations/{operation_id}");
        let manifest = ApplyManifest {
            contract_version: 1,
            operation_id,
            dataset_id: dataset,
            asset_id: asset,
            asset_revision: revision,
            image_path: target.image.image.path,
            base_image_digest: preview.base_image_digest,
            base_annotation_digest: preview.base_annotation_digest,
            label_schema_digest: preview.label_schema_digest,
            candidate_ref: preview.candidate_ref,
            payload_path: format!("{directory}/image.payload"),
            region: preview.region,
            options: preview.options,
            changed_pixels: preview.changed_pixels,
            lossy_reencode: preview.lossy_reencode,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|error| ColorApplyError::TaskEvidence(Box::new(error)))?;
        if manifest_bytes.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid());
        }
        let manifest_path = format!("{directory}/operation.json");
        let manifest_digest = digest(&manifest_bytes);
        let input = freeze_input_manifest(&self.previews.contracts, json!({
            "dataset_id":dataset, "label_schema_digest":manifest.label_schema_digest,
            "assets":[{"asset_id":asset,"media_digest":manifest.base_image_digest,
                "annotation_digest":manifest.base_annotation_digest,"source_metadata_digest":null}],
            "capability_id":CAPABILITY,"implementation_version":env!("CARGO_PKG_VERSION"),
            "model_identity":null,"resolved_config":{
                "preview_task_id":preview_task,"candidate_artifact_id":manifest.candidate_ref.artifact_id,
                "operation_manifest_digest":manifest_digest,"expected_asset_revision":revision.to_string(),
            },
        })).map_err(ColorApplyError::Fingerprint)?;
        let input_path = PathBuf::from(format!("{directory}/input.json"));
        publish_immutable(&target.root, Path::new(&manifest.payload_path), &bytes)?;
        publish_immutable(&target.root, Path::new(&manifest_path), &manifest_bytes)?;
        publish_immutable(&target.root, &input_path, &input.bytes)?;
        let submission = TaskSubmission {
            workspace_session: session,
            dataset_id: Some(dataset),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key,
            operation_kind: KIND.into(),
            capability_id: CAPABILITY.into(),
            input_manifest_path: input_path,
            request_fingerprint: fingerprint,
            input_fingerprint: input.input_fingerprint,
        };
        let task_id = self.owner.workspace_action(session, move |workspace| {
            let tx = workspace.connection.transaction().map_err(database)?;
            let task = enqueue_task_in(&tx, &submission, now).map_err(WorkspaceThreadError::Task)?;
            tx.execute(
                "INSERT INTO file_operations(operation_id,idempotency_key,request_id,request_fingerprint,workspace_session_id,
                    operation_kind,dataset_id,asset_id,task_id,status,manifest_path,manifest_digest,created_at_ms,updated_at_ms)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'PREPARED',?10,?11,?12,?12)",
                params![operation_id.to_string(),submission.idempotency_key.to_string(),submission.request_id.to_string(),
                    format_digest(&fingerprint),session.to_string(),KIND,dataset.to_string(),asset.to_string(),
                    task.task_id.to_string(),manifest_path,manifest_digest,now],
            ).map_err(database)?;
            tx.commit().map_err(database)?;
            Ok(task.task_id)
        })?;
        Ok(ColorApplyOperation {
            operation_id,
            task_id,
            state: ColorApplyState::Prepared,
        })
    }

    pub fn snapshot(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ColorApplyOperation, ColorApplyError> {
        self.owner
            .workspace_action(session, move |workspace| {
                workspace
                    .connection
                    .query_row(
                        &format!("{SELECT_OPERATION} AND task_id=?2"),
                        params![KIND, task.to_string()],
                        StoredOperation::read,
                    )
                    .optional()
                    .map_err(database)?
                    .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
            })?
            .receipt()
    }

    pub fn resume(
        &self,
        session: Uuid,
        operation: Uuid,
        now: i64,
    ) -> Result<ColorApplyOperation, ColorApplyError> {
        let _guard = self.owner.lock_final_operations()?;
        let root = self.owner.storage_root(session)?;
        let record = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    &format!("{SELECT_OPERATION} AND operation_id=?2"),
                    params![KIND, operation.to_string()],
                    StoredOperation::read,
                )
                .optional()
                .map_err(database)?
                .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
        })?;
        let receipt = record.receipt()?;
        if receipt.state != ColorApplyState::Prepared {
            return Ok(receipt);
        }
        let directory = format!("artifacts/file-operations/{operation}");
        if record.path != format!("{directory}/operation.json") {
            return Err(ColorApplyError::ResultUnconfirmed);
        }
        let envelope = read_scoped(&root, Path::new(&record.path), MANIFEST_LIMIT)?;
        if digest(&envelope) != record.sha256 {
            return Err(ColorApplyError::ResultUnconfirmed);
        }
        let document = strict_json(&envelope)
            .map_err(|error| ColorApplyError::TaskEvidence(Box::new(error)))?;
        let manifest: ApplyManifest = serde_json::from_value(document)
            .map_err(|error| ColorApplyError::TaskEvidence(Box::new(error)))?;
        if manifest.contract_version != 1
            || manifest.operation_id != operation
            || manifest.dataset_id != record.dataset
            || manifest.asset_id != record.asset
            || manifest.payload_path != format!("{directory}/image.payload")
        {
            return Err(ColorApplyError::ResultUnconfirmed);
        }
        let payload = read_scoped(&root, Path::new(&manifest.payload_path), MAX_IMAGE_BYTES)?;
        if digest(&payload) != manifest.candidate_ref.sha256 {
            return Err(ColorApplyError::ResultUnconfirmed);
        }
        let task = record.task;
        self.owner.workspace_action(session, move |workspace| {
            workspace.connection.execute(
                "UPDATE tasks SET status='RUNNING',error_code=NULL,finished_at_ms=NULL,started_at_ms=?2,updated_at_ms=?2,
                 row_revision=row_revision+1 WHERE task_id=?1 AND status IN ('QUEUED','INTERRUPTED')",
                params![task.to_string(),now],
            ).map_err(database)?;
            Ok(())
        })?;
        let target = self.target(session, record.dataset, record.asset)?;
        let matches = target.revision == manifest.asset_revision
            && target.image.image.path == manifest.image_path
            && target.image.image.sha256 == manifest.base_image_digest
            && target.image.annotation.as_ref().map(|file| &file.sha256)
                == manifest.base_annotation_digest.as_ref()
            && target.image.label_schema_fingerprint == manifest.label_schema_digest
            && current_annotation(&target.image)?;
        let committed = matches
            && replace_verified_file(
                &manifest.image_path,
                Some(&manifest.base_image_digest),
                &manifest.candidate_ref.sha256,
                &payload,
                MAX_IMAGE_BYTES,
            )?;
        self.record(session, record, manifest, committed, now)?;
        Ok(ColorApplyOperation {
            state: if committed {
                ColorApplyState::Committed
            } else {
                ColorApplyState::Conflict
            },
            ..receipt
        })
    }

    fn target(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
    ) -> Result<ApplyTarget, ColorApplyError> {
        Ok(self.owner.workspace_action(session, move |workspace| {
            let image = workspace.registered_image(dataset, asset).map_err(WorkspaceThreadError::Dataset)?;
            let (revision, pending) = workspace.connection.query_row(
                "SELECT row_revision,EXISTS(SELECT 1 FROM file_operations f WHERE f.asset_id=a.asset_id AND f.status='PREPARED')
                 FROM assets a WHERE asset_id=?1", [asset.to_string()], |row|Ok((row.get(0)?,row.get(1)?)),
            ).map_err(database)?;
            Ok(ApplyTarget { root:workspace.root().to_owned(),image,revision,pending })
        })?)
    }

    fn record(
        &self,
        session: Uuid,
        record: StoredOperation,
        manifest: ApplyManifest,
        committed: bool,
        now: i64,
    ) -> Result<(), ColorApplyError> {
        self.owner.workspace_action(session, move |workspace| {
            let tx = workspace.connection.transaction().map_err(database)?;
            let (operation_status,task_status,error_code,result) = if committed {
                expect_one(tx.execute(
                    "UPDATE assets SET media_digest=?1,row_revision=row_revision+1,updated_at_ms=?2
                     WHERE asset_id=?3 AND row_revision=?4 AND media_digest=?5 AND annotation_digest IS ?6",
                    params![manifest.candidate_ref.sha256,now,record.asset.to_string(),manifest.asset_revision,
                        manifest.base_image_digest,manifest.base_annotation_digest],
                ).map_err(database)?)?;
                tx.execute(
                    "UPDATE review_sessions SET status='STALE',row_revision=row_revision+1,updated_at_ms=?2
                     WHERE asset_id=?1 AND status IN ('OPEN','CONFLICT','CONFIRMED')",
                    params![record.asset.to_string(),now],
                ).map_err(database)?;
                tx.execute(
                    "UPDATE staging_items SET check_task_id=NULL,check_acceptance_digest=NULL,check_accepted_at_ms=NULL
                     WHERE asset_id=?1",
                    [record.asset.to_string()],
                ).map_err(database)?;
                ("COMMITTED","SUCCEEDED",None,Some(json!({
                    "operation_id":record.id,"asset_id":record.asset,
                    "old_image_digest":manifest.base_image_digest,"image_digest":manifest.candidate_ref.sha256,
                    "candidate_ref":manifest.candidate_ref,
                }).to_string()))
            } else {
                ("CONFLICT","FAILED",Some("INPUT_CHANGED"),None)
            };
            expect_one(tx.execute(
                "UPDATE file_operations SET status=?1,result_json=?2,error_code=?3,completed_at_ms=?4,updated_at_ms=?4
                 WHERE operation_id=?5 AND operation_kind=?6 AND status='PREPARED'",
                params![operation_status,result,error_code,now,record.id.to_string(),KIND],
            ).map_err(database)?)?;
            expect_one(tx.execute(
                "UPDATE tasks SET status=?1,error_code=?2,finished_at_ms=?3,updated_at_ms=?3,row_revision=row_revision+1
                 WHERE task_id=?4 AND operation_kind=?5 AND status='RUNNING'",
                params![task_status,error_code,now,record.task.to_string(),KIND],
            ).map_err(database)?)?;
            tx.commit().map_err(database)
        })?;
        Ok(())
    }
}

impl StoredOperation {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row_uuid(row, 0)?,
            task: row_uuid(row, 1)?,
            status: row.get(2)?,
            dataset: row_uuid(row, 3)?,
            asset: row_uuid(row, 4)?,
            path: row.get(5)?,
            sha256: row.get(6)?,
            fingerprint: row.get(7)?,
        })
    }

    fn receipt(&self) -> Result<ColorApplyOperation, ColorApplyError> {
        let state = match self.status.as_str() {
            "PREPARED" => ColorApplyState::Prepared,
            "COMMITTED" => ColorApplyState::Committed,
            "CONFLICT" => ColorApplyState::Conflict,
            _ => return Err(ColorApplyError::ResultUnconfirmed),
        };
        Ok(ColorApplyOperation {
            operation_id: self.id,
            task_id: self.task,
            state,
        })
    }
}

fn current_annotation(binding: &RegisteredImage) -> std::io::Result<bool> {
    binding.annotation.as_ref().map_or(Ok(true), |file| {
        registered_digest(&file.path, ANNOTATION_LIMIT)
            .map(|actual| actual.as_deref() == Some(file.sha256.as_str()))
    })
}

fn expect_one(rows: usize) -> Result<(), WorkspaceThreadError> {
    if rows != 1 {
        return Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict));
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
