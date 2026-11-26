use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Row, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    CocoExportError, CocoExportPolicy, CocoExportReceipt, CocoExportRequest, CocoExportService,
};
use crate::dataset_query::DatasetQueryError;
use crate::input_fingerprint::{FrozenInputManifest, freeze_input_manifest};
use crate::paths::publish_immutable;
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

pub(super) const KIND: &str = "DATASET_EXPORT";
const CAPABILITY: &str = "dataset_export_coco.v1";

pub struct CocoExportSubmission {
    pub request: CocoExportRequest,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

pub struct CocoExportJob {
    task_id: Uuid,
    request: CocoExportRequest,
    destination: PathBuf,
    input_fingerprint: [u8; 32],
}

#[derive(Debug)]
pub struct CocoExportTaskSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub accepted_at_ms: i64,
    pub receipt: Option<CocoExportReceipt>,
}

impl CocoExportService {
    pub fn submit(
        &self,
        session: Uuid,
        submission: CocoExportSubmission,
        now_ms: i64,
    ) -> Result<(CocoExportTaskSnapshot, Option<CocoExportJob>), CocoExportError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(CocoExportError::Storage)?;
        let request = submission.request;
        let key = submission.idempotency_key.to_string();
        let fingerprint: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&(
                CAPABILITY,
                request.dataset_id,
                request.destination_grant_id,
                policy_text(request.policy),
            ))
            .map_err(CocoExportError::Json)?,
        )
        .into();
        let existing = self.owner.workspace_action(session, move |owner| {
            owner.connection.query_row(
                "SELECT task_id, request_fingerprint FROM tasks WHERE operation_kind=?1 AND idempotency_key=?2",
                params![KIND, key], |row| Ok((row_uuid(row, 0)?, row.get::<_, String>(1)?)),
            ).optional().map_err(database)
        }).map_err(CocoExportError::Storage)?;
        if let Some((task, original)) = existing {
            if original != format_digest(&fingerprint) {
                return Err(CocoExportError::Storage(WorkspaceThreadError::Task(
                    TaskStoreError::IdempotencyConflict,
                )));
            }
            let snapshot = self
                .task_snapshot(session, task, now_ms)?
                .ok_or(CocoExportError::InvalidTask)?;
            return Ok((snapshot, None));
        }

        let input = self.freeze(session, &request)?;
        let root = self
            .owner
            .storage_root(session)
            .map_err(CocoExportError::Storage)?;
        let destination = self
            .owner
            .consume_export_directory(session, request.destination_grant_id)
            .map_err(CocoExportError::Storage)?;
        let relative = Path::new("artifacts/task-inputs")
            .join(format!("{}.json", format_digest(&input.sha256)));
        publish_immutable(&root, &relative, &input.bytes)?;
        let stored = self
            .owner
            .enqueue_task(
                TaskSubmission {
                    workspace_session: session,
                    dataset_id: Some(request.dataset_id),
                    request_id: submission.request_id,
                    idempotency_key: submission.idempotency_key,
                    operation_kind: KIND.into(),
                    capability_id: CAPABILITY.into(),
                    input_manifest_path: relative,
                    request_fingerprint: fingerprint,
                    input_fingerprint: input.input_fingerprint,
                },
                now_ms,
            )
            .map_err(CocoExportError::Storage)?;
        let task_id = stored.task_id;
        Ok((
            CocoExportTaskSnapshot {
                task_id,
                status: stored.status,
                error_code: None,
                accepted_at_ms: stored.created_at_ms,
                receipt: None,
            },
            Some(CocoExportJob {
                task_id,
                request,
                destination,
                input_fingerprint: input.input_fingerprint,
            }),
        ))
    }

    pub fn run_admitted(
        &self,
        session: Uuid,
        job: CocoExportJob,
        now: impl Fn() -> i64,
    ) -> Result<(), CocoExportError> {
        let _guard = self
            .owner
            .lock_final_operations()
            .map_err(CocoExportError::Storage)?;
        let start = now();
        let task = job.task_id.to_string();
        let granted = session.to_string();
        let claimed = self.owner.workspace_action(session, move |owner| {
            owner.connection.execute(
                "UPDATE tasks SET status='RUNNING', started_at_ms=?4, updated_at_ms=?4, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'",
                params![task, KIND, granted, start],
            ).map_err(database)
        }).map_err(CocoExportError::Storage)?;
        if claimed != 1 {
            return Err(CocoExportError::InvalidTask);
        }
        let result = self.freeze(session, &job.request).and_then(|current| {
            if current.input_fingerprint != job.input_fingerprint {
                return Err(CocoExportError::InputChanged);
            }
            self.export_to(
                session,
                job.request,
                &job.destination,
                job.task_id,
                |stage, receipt| {
                    self.prepare_publication(session, job.task_id, stage, receipt, now())
                },
            )
        });
        let (status, error, payload) = match result {
            Ok(receipt) => (
                "SUCCEEDED",
                None,
                serde_json::to_string(&receipt).map_err(CocoExportError::Json)?,
            ),
            Err(error) => ("FAILED", Some(error_code(&error)), "{}".to_owned()),
        };
        let finished = now();
        let updated = self
            .owner
            .workspace_action(session, move |owner| {
                let transaction = owner.connection.transaction().map_err(database)?;
                transaction
                    .execute(
                        "UPDATE tasks SET status=?3, error_code=?4, execution_metadata_json=?5,
                     finished_at_ms=?6, updated_at_ms=?6, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND status='RUNNING'",
                        params![
                            job.task_id.to_string(),
                            KIND,
                            status,
                            error,
                            payload,
                            finished
                        ],
                    )
                    .map_err(database)?;
                super::publication::finish_publication(
                    &transaction,
                    job.task_id,
                    status,
                    error,
                    &payload,
                    finished,
                )?;
                transaction.commit().map_err(database)
            })
            .map_err(CocoExportError::Storage);
        if updated.is_err() {
            let _ = self.owner.workspace_action(session, move |owner| {
                owner.connection.execute(
                    "UPDATE tasks SET status='INTERRUPTED',error_code='EXPORT_RECOVERY_REQUIRED',
                        finished_at_ms=?3,updated_at_ms=?3,row_revision=row_revision+1
                     WHERE task_id=?1 AND operation_kind=?2 AND status='RUNNING'",
                    params![job.task_id.to_string(), KIND, finished],
                ).map(|_| ()).map_err(database)
            });
        }
        updated
    }

    pub fn task_snapshot(
        &self,
        session: Uuid,
        task: Uuid,
        now_ms: i64,
    ) -> Result<Option<CocoExportTaskSnapshot>, CocoExportError> {
        self.recover_publication(session, task, now_ms)?;
        self.owner
            .workspace_action(session, move |owner| {
                let task = task.to_string();
                owner
                    .connection
                    .execute(
                        "UPDATE tasks SET status='INTERRUPTED', error_code='SESSION_ENDED',
                     finished_at_ms=?4, updated_at_ms=?4, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3
                   AND status IN ('QUEUED','RUNNING')",
                        params![task, KIND, session.to_string(), now_ms],
                    )
                    .map_err(database)?;
                owner
                    .connection
                    .query_row(
                        "SELECT task_id, status, error_code, created_at_ms, execution_metadata_json
                 FROM tasks WHERE task_id=?1 AND operation_kind=?2",
                        params![task, KIND],
                        decode,
                    )
                    .optional()
                    .map_err(database)
            })
            .map_err(CocoExportError::Storage)?
            .map(|(mut snapshot, payload)| {
                if snapshot.status == "SUCCEEDED" {
                    snapshot.receipt =
                        Some(serde_json::from_str(&payload).map_err(CocoExportError::Json)?);
                }
                Ok(snapshot)
            })
            .transpose()
    }

    fn freeze(
        &self,
        session: Uuid,
        request: &CocoExportRequest,
    ) -> Result<FrozenInputManifest, CocoExportError> {
        let dataset = request.dataset_id;
        let (categories_digest, assets, missing) = self.owner.workspace_action(session, move |owner| {
            let dataset = dataset.to_string();
            let categories: String = owner.connection.query_row(
                "SELECT categories_json FROM datasets WHERE dataset_id=?1", [&dataset], |row| row.get(0),
            ).optional().map_err(database)?.ok_or(WorkspaceThreadError::Dataset(DatasetQueryError::UnknownDataset))?;
            let mut statement = owner.connection.prepare(
                "SELECT m.asset_id, a.media_digest, a.annotation_digest, a.row_revision, a.availability,
                    m.final_review_id, (SELECT status FROM review_sessions r WHERE r.review_id=m.final_review_id),
                    (SELECT review_id FROM review_sessions r WHERE r.asset_id=m.asset_id AND r.status IN ('OPEN','COMMITTING','CONFLICT'))
                 FROM dataset_assets m JOIN assets a USING(asset_id) WHERE m.dataset_id=?1 ORDER BY m.asset_id",
            ).map_err(database)?;
            let mut rows = statement.query([&dataset]).map_err(database)?;
            let mut assets = Vec::new();
            let mut missing = Vec::new();
            while let Some(row) = rows.next().map_err(database)? {
                let asset: String = row.get(0).map_err(database)?;
                let media: Option<String> = row.get(1).map_err(database)?;
                let annotation: Option<String> = row.get(2).map_err(database)?;
                let record = (
                    row.get::<_, i64>(3).map_err(database)?,
                    row.get::<_, String>(4).map_err(database)?,
                    row.get::<_, Option<String>>(5).map_err(database)?,
                    row.get::<_, Option<String>>(6).map_err(database)?,
                    row.get::<_, Option<String>>(7).map_err(database)?,
                );
                let metadata = format!("{:x}", Sha256::digest(json!(record).to_string().as_bytes()));
                if let Some(media) = media {
                    assets.push(json!({"asset_id": asset, "media_digest": media, "annotation_digest": annotation, "source_metadata_digest": metadata}));
                } else {
                    missing.push(json!([asset, metadata]));
                }
            }
            Ok((format!("{:x}", Sha256::digest(categories.as_bytes())), assets, missing))
        }).map_err(CocoExportError::Storage)?;
        let input: Value = json!({
            "capability_id": CAPABILITY, "implementation_version": "1",
            "dataset_id": dataset, "label_schema_digest": categories_digest, "assets": assets, "model_identity": null,
            "resolved_config": {"format": "COCO_DETECTION", "policy": policy_text(request.policy),
                "destination_grant_id": request.destination_grant_id, "missing_media": missing},
        });
        freeze_input_manifest(&self.contracts, input).map_err(|_| CocoExportError::InvalidTask)
    }
}

fn decode(row: &Row<'_>) -> rusqlite::Result<(CocoExportTaskSnapshot, String)> {
    Ok((
        CocoExportTaskSnapshot {
            task_id: row_uuid(row, 0)?,
            status: row.get(1)?,
            error_code: row.get(2)?,
            accepted_at_ms: row.get(3)?,
            receipt: None,
        },
        row.get(4)?,
    ))
}

fn policy_text(policy: CocoExportPolicy) -> &'static str {
    match policy {
        CocoExportPolicy::Strict => "STRICT",
        CocoExportPolicy::ExcludeIneligible => "EXCLUDE_INELIGIBLE",
    }
}

pub(super) fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}

fn error_code(error: &CocoExportError) -> &'static str {
    match error {
        CocoExportError::Ineligible(_) => "EXPORT_BLOCKED",
        CocoExportError::NoEligibleImages => "NO_ELIGIBLE_IMAGES",
        CocoExportError::InputChanged => "INPUT_CHANGED",
        _ => "EXPORT_FAILED",
    }
}
