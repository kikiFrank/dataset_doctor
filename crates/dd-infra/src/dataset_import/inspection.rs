//! Read-only import preflight tasks and their portable summaries.

use std::collections::BTreeSet;
use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::confirmation::{database, find_request};
use super::{Annotation, DatasetImportCoordinator, DatasetImportError, ImportPlan};
use crate::import_preflight::ImportProblem;
use crate::input_fingerprint::freeze_input_manifest;
use crate::paths::publish_immutable;
use crate::task_store::{TaskStoreError, TaskSubmission, format_digest};
use crate::workspace_thread::WorkspaceThreadError;

const OPERATION: &str = "DATASET_IMPORT_INSPECT";
const CAPABILITY: &str = "dataset_import_inspect.v1";

#[derive(Clone, Debug)]
pub struct ImportInspection {
    pub directory_grant_id: Uuid,
    pub dataset_name: String,
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportPreviewFile {
    pub image_path: String,
    pub media_sha256: Option<String>,
    pub annotation_state: String,
    pub annotation_path: Option<String>,
    pub annotation_sha256: Option<String>,
    pub issue_code: Option<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportPreviewResult {
    pub plan_id: String,
    pub plan_fingerprint: String,
    pub dataset_name: String,
    pub category_names: Vec<String>,
    pub files: Vec<ImportPreviewFile>,
    pub can_confirm: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ImportInspectionSnapshot {
    pub task_id: Uuid,
    pub status: String,
    pub accepted_at_ms: i64,
    pub preview: Option<ImportPreviewResult>,
    pub error_code: Option<String>,
}

impl DatasetImportCoordinator {
    pub fn submit_inspection(
        &self,
        session: Uuid,
        request: ImportInspection,
        now_ms: i64,
    ) -> Result<ImportInspectionSnapshot, DatasetImportError> {
        let config = json!({
            "directory_grant_id": request.directory_grant_id.to_string(),
            "dataset_name": request.dataset_name.trim(),
            "format": "COCO_IMAGE_BBOX",
        });
        let mut sha = Sha256::new();
        sha.update(b"datasetdoctor.dataset_import_inspect.v1\0");
        sha.update(config.to_string().as_bytes());
        let fingerprint: [u8; 32] = sha.finalize().into();
        if let Some(task) = self
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
            return self.inspection_task(session, task, now_ms);
        }

        let manifest = freeze_input_manifest(
            &self.contracts,
            json!({
                "capability_id": CAPABILITY,
                "implementation_version": "1",
                "dataset_id": null,
                "label_schema_digest": null,
                "assets": [],
                "model_identity": null,
                "resolved_config": config,
            }),
        )
        .map_err(|error| DatasetImportError::Source(std::io::Error::other(error)))?;
        let path = PathBuf::from(format!(
            "artifacts/import/{}.json",
            format_digest(&manifest.sha256)
        ));
        let root = self
            .workspace
            .storage_root(session)
            .map_err(DatasetImportError::Workspace)?;
        publish_immutable(&root, &path, &manifest.bytes).map_err(DatasetImportError::Source)?;
        let task = self
            .workspace
            .enqueue_task(
                TaskSubmission {
                    workspace_session: session,
                    dataset_id: None,
                    request_id: request.request_id,
                    idempotency_key: request.idempotency_key,
                    operation_kind: OPERATION.into(),
                    capability_id: CAPABILITY.into(),
                    input_manifest_path: path,
                    request_fingerprint: fingerprint,
                    input_fingerprint: manifest.input_fingerprint,
                },
                now_ms,
            )
            .map_err(DatasetImportError::Workspace)?;
        self.inspection_task(session, task.task_id, now_ms)
    }

    pub fn run_inspection(
        &self,
        session: Uuid,
        request: ImportInspection,
        now_ms: i64,
    ) -> Result<ImportInspectionSnapshot, DatasetImportError> {
        let task = self.submit_inspection(session, request.clone(), now_ms)?;
        if task.status != "QUEUED" {
            return Ok(task);
        }
        let id = task.task_id;
        let attempt = Uuid::new_v4();
        let claimed = self.workspace.workspace_action(session, move |workspace| {
            workspace.connection.execute(
                "UPDATE tasks SET status='RUNNING', current_attempt_id=?4, attempt_no=attempt_no+1,
                    started_at_ms=?5, updated_at_ms=?5, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3 AND status='QUEUED'",
                params![id.to_string(), OPERATION, session.to_string(), attempt.to_string(), now_ms],
            ).map_err(database)
        }).map_err(DatasetImportError::Workspace)?;
        if claimed == 0 {
            return self.inspection_task(session, id, now_ms);
        }
        let result = self.inspect_directory(
            session,
            request.directory_grant_id,
            request.dataset_name.clone(),
        );
        let (status, preview, error_code) = match result {
            Ok(plan) => (
                "SUCCEEDED",
                Some(summarize(plan, request.dataset_name.trim().to_owned())),
                None,
            ),
            Err(DatasetImportError::Workspace(_)) => {
                ("FAILED", None, Some("DIRECTORY_GRANT_INVALID"))
            }
            Err(_) => ("FAILED", None, Some("IMPORT_PREVIEW_FAILED")),
        };
        self.workspace.workspace_action(session, move |workspace| {
            let encoded = serde_json::to_string(&preview)
                .map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Receipt(error)))?;
            workspace.connection.execute(
                "UPDATE tasks SET status=?5, error_code=?6,
                    progress=CASE WHEN ?5='SUCCEEDED' THEN 1 ELSE NULL END,
                    execution_metadata_json=json_set(execution_metadata_json, '$.import_preview', json(?7)),
                    finished_at_ms=?8, updated_at_ms=?8, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id=?3
                   AND current_attempt_id=?4 AND status='RUNNING'",
                params![id.to_string(), OPERATION, session.to_string(), attempt.to_string(),
                    status, error_code, encoded, now_ms],
            ).map_err(database)
        }).map_err(DatasetImportError::Workspace)?;
        self.inspection_task(session, id, now_ms)
    }

    pub fn inspection_task(
        &self,
        session: Uuid,
        task_id: Uuid,
        now_ms: i64,
    ) -> Result<ImportInspectionSnapshot, DatasetImportError> {
        self.workspace.workspace_action(session, move |workspace| {
            let session = session.to_string();
            workspace.connection.execute(
                "UPDATE tasks SET status='INTERRUPTED', error_code='SESSION_ENDED',
                    finished_at_ms=?4, updated_at_ms=?4, row_revision=row_revision+1
                 WHERE task_id=?1 AND operation_kind=?2 AND workspace_session_id<>?3
                   AND status IN ('QUEUED', 'RUNNING')",
                params![task_id.to_string(), OPERATION, session, now_ms],
            ).map_err(database)?;
            let (status, encoded, error_code, accepted_at_ms, origin): (String, Option<String>, Option<String>, i64, String) =
                workspace.connection.query_row(
                    "SELECT status, json_extract(execution_metadata_json, '$.import_preview'), error_code,
                         created_at_ms, workspace_session_id FROM tasks WHERE task_id=?1 AND operation_kind=?2",
                    params![task_id.to_string(), OPERATION],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                ).optional().map_err(database)?.ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))?;
            let mut preview: Option<ImportPreviewResult> = encoded.as_deref().map(serde_json::from_str).transpose()
                .map_err(|error| WorkspaceThreadError::Task(TaskStoreError::Receipt(error)))?;
            if origin != session {
                if let Some(preview) = &mut preview {
                    preview.can_confirm = false;
                }
            }
            Ok(ImportInspectionSnapshot { task_id, status, accepted_at_ms, preview, error_code })
        }).map_err(DatasetImportError::Workspace)
    }
}

fn summarize(plan: ImportPlan, dataset_name: String) -> ImportPreviewResult {
    let mut category_names = BTreeSet::new();
    let mut can_confirm = true;
    let files = plan
        .preview
        .inputs
        .iter()
        .map(|file| {
            let mut item = ImportPreviewFile {
                image_path: file.image.to_string_lossy().replace('\\', "/"),
                media_sha256: file.media_sha256.clone(),
                annotation_state: "MISSING".into(),
                annotation_path: None,
                annotation_sha256: None,
                issue_code: None,
            };
            match &file.annotation {
                Annotation::Missing => {}
                Annotation::Reference {
                    path,
                    sha256,
                    categories,
                    ..
                } => {
                    item.annotation_state = "REFERENCE".into();
                    item.annotation_path = Some(path.to_string_lossy().replace('\\', "/"));
                    item.annotation_sha256 = Some(sha256.clone());
                    category_names.extend(categories.iter().map(|category| category.name.clone()));
                }
                Annotation::Blocked(problem) => {
                    can_confirm = false;
                    item.annotation_state = "BLOCKED".into();
                    item.issue_code = Some(
                        match problem {
                            ImportProblem::ImageRead(_) => "IMAGE_UNREADABLE",
                            ImportProblem::AnnotationRead(_) => "ANNOTATION_UNREADABLE",
                            ImportProblem::AmbiguousSidecar => "AMBIGUOUS_SIDECAR",
                            ImportProblem::WrongImage => "WRONG_IMAGE",
                            ImportProblem::AnnotationTooLarge => "ANNOTATION_TOO_LARGE",
                            ImportProblem::UnsupportedAnnotation(_) => "UNSUPPORTED_ANNOTATION",
                        }
                        .into(),
                    );
                }
            }
            item
        })
        .collect();
    ImportPreviewResult {
        plan_id: plan.plan_id.to_string(),
        plan_fingerprint: format_digest(&plan.fingerprint),
        dataset_name,
        category_names: category_names.into_iter().collect(),
        files,
        can_confirm,
    }
}
