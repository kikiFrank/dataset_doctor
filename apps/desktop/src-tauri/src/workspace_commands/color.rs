#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;

use dd_infra::artifact_reference::{
    ArtifactOwner as StoredOwner, ArtifactReference as StoredReference,
};
use dd_infra::color_apply::{ColorApplyService, ColorApplyState, ColorApplySubmission};
use dd_infra::color_preview as preview;
use dd_infra::color_preview::{
    ColorPreviewError, ColorPreviewJob, ColorPreviewRuntime, ColorPreviewService,
};
use dd_infra::task_store::TaskStoreError;
use dd_infra::worker_process::WorkerProgram;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::ArtifactOwner;
use dd_ui_contracts::color::{
    ColorApplySubmitRequest, ColorApplyView, ColorHueGate, ColorMethod, ColorOptions,
    ColorPreviewSubmitRequest, ColorPreviewView, ColorRegion,
};
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::workspace::AppError;
use tauri::State;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Commands, run};

pub(crate) struct ColorRunner {
    program: WorkerProgram,
}

impl ColorRunner {
    #[cfg(test)]
    pub(crate) fn new(interpreter: PathBuf, entrypoint: PathBuf) -> Arc<Self> {
        Self::from_program(WorkerProgram::Script {
            interpreter,
            entrypoint,
        })
    }

    fn from_program(program: WorkerProgram) -> Arc<Self> {
        Arc::new(Self { program })
    }

    pub(crate) fn configured(program: Option<WorkerProgram>) -> Option<Arc<Self>> {
        program.map(Self::from_program)
    }

    async fn execute(
        self: Arc<Self>,
        owner: Arc<WorkspaceThread>,
        session: Uuid,
        job: ColorPreviewJob,
    ) {
        let task = job.task_id();
        let startup_failed = match ColorPreviewRuntime::launch_with_program(
            Arc::clone(&owner),
            session,
            self.program.clone(),
        )
        .await
        {
            Ok(mut worker) => {
                if worker.run(job).await.is_ok() {
                    let _ = worker.shutdown().await;
                    return;
                }
                if worker.terminate().await.is_err() {
                    return;
                }
                false
            }
            Err(_) => true,
        };
        let _ = tauri::async_runtime::spawn_blocking(move || {
            ColorPreviewRuntime::record_unavailable(&owner, session, task, startup_failed)
        })
        .await;
    }
}

#[tauri::command]
pub async fn repair_color_preview(
    request: ColorPreviewSubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runner: State<'_, Option<Arc<ColorRunner>>>,
) -> Result<ImportOperationRef, AppError> {
    let runner = runner.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.start_color_preview(runner, request),
    )
    .await
}

#[tauri::command]
pub async fn repair_color_apply(
    request: ColorApplySubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ImportOperationRef, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.start_color_apply(request),
    )
    .await
}

impl Commands {
    pub(super) fn start_color_preview(
        &self,
        runner: Option<Arc<ColorRunner>>,
        request: ColorPreviewSubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let service = ColorPreviewService::new(Arc::clone(&self.thread))
            .map_err(|error| self.color_error(error))?;
        let (mut task, job) = service
            .submit(
                session,
                preview::ColorPreviewSubmission {
                    request_id,
                    idempotency_key,
                    dataset_id: self.parse_uuid(request.dataset_id)?,
                    asset_id: self.parse_uuid(request.asset_id)?,
                    base_image_digest: request.base_image_digest,
                    region: core_region(request.region),
                    options: core_options(request.operation),
                },
                self.now_ms()?,
            )
            .map_err(|error| self.color_error(error))?;
        if let Some(job) = job {
            if let Some(runner) = runner {
                tauri::async_runtime::spawn(runner.execute(Arc::clone(&self.thread), session, job));
            } else {
                ColorPreviewRuntime::record_unavailable(&self.thread, session, job.task_id(), true)
                    .map_err(|error| self.color_error(error))?;
                task = service
                    .snapshot(session, task.task_id)
                    .map_err(|error| self.color_error(error))?;
            }
        }
        self.import_operation(
            ImportOperationKind::ColorPreview,
            task.task_id,
            task.status,
            task.accepted_at_ms,
        )
    }

    pub(super) fn start_color_apply(
        &self,
        request: ColorApplySubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let reference = request.candidate_ref;
        let ArtifactOwner::Task(task) = reference.owner else {
            return Err(self.error("INVALID_ARGUMENT", "请选择已完成的颜色候选。"));
        };
        let task_id = self.parse_uuid(task)?;
        let service = ColorApplyService::new(Arc::clone(&self.thread))
            .map_err(|error| self.color_error(error))?;
        let mut operation = service
            .prepare(
                session,
                ColorApplySubmission {
                    request_id,
                    idempotency_key,
                    dataset_id: self.parse_uuid(request.dataset_id)?,
                    asset_id: self.parse_uuid(request.asset_id)?,
                    base_image_digest: request.base_image_digest,
                    expected_asset_revision: request.expected_asset_revision.into(),
                    candidate_ref: StoredReference {
                        artifact_id: self.parse_uuid(reference.artifact_id)?,
                        owner: StoredOwner::Task(task_id),
                        kind: reference.kind,
                        media_type: reference.media_type,
                        sha256: reference.sha256,
                        size_bytes: reference.size_bytes.into(),
                        schema_id: reference.schema_id,
                    },
                },
                self.now_ms()?,
            )
            .map_err(|error| self.color_error(error))?;
        if operation.state == ColorApplyState::Prepared {
            service
                .queue_resume(session, operation.operation_id, self.now_ms()?)
                .map_err(|error| self.color_error(error))?;
            operation = service
                .snapshot(session, operation.task_id)
                .map_err(|error| self.color_error(error))?;
            let id = operation.operation_id;
            tauri::async_runtime::spawn_blocking(move || resolve_color(&service, session, id));
        }
        self.import_operation(
            ImportOperationKind::ColorApply,
            operation.task_id,
            operation.task_status,
            operation.accepted_at_ms,
        )
    }

    pub(super) fn color_preview_task(
        &self,
        session: Uuid,
        id: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let task = ColorPreviewService::new(Arc::clone(&self.thread))
            .and_then(|service| service.snapshot(session, id))
            .map_err(|error| self.color_error(error))?;
        let mut view = self.color_task(
            ImportOperationKind::ColorPreview,
            id,
            task.status,
            task.error_code,
        )?;
        view.color_preview = task
            .preview
            .map(|result| {
                Ok::<_, AppError>(ColorPreviewView {
                    dataset_id: self.uuid_text(result.dataset_id.to_string())?,
                    asset_id: self.uuid_text(result.asset_id.to_string())?,
                    asset_revision: result.asset_revision.into(),
                    base_image_digest: result.base_image_digest,
                    candidate_ref: self.wire_artifact(result.candidate_ref)?,
                    mask_ref: self.wire_artifact(result.mask_ref)?,
                    changed_pixels: result.changed_pixels.into(),
                    lossy_reencode: result.lossy_reencode,
                    region: wire_region(result.region),
                    operation: wire_options(result.options),
                })
            })
            .transpose()?;
        Ok(view)
    }

    pub(super) fn color_apply_task(
        &self,
        session: Uuid,
        id: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let task = ColorApplyService::new(Arc::clone(&self.thread))
            .and_then(|service| service.snapshot(session, id))
            .map_err(|error| self.color_error(error))?;
        let mut view = self.color_task(
            ImportOperationKind::ColorApply,
            id,
            task.task_status,
            task.error_code,
        )?;
        if let Some(image_digest) = task.image_digest {
            view.color_apply = Some(ColorApplyView {
                operation_id: self.uuid_text(task.operation_id.to_string())?,
                asset_id: self.uuid_text(task.asset_id.to_string())?,
                image_digest,
            });
        }
        Ok(view)
    }

    pub(super) fn resume_pending_colors(&self, session: Uuid) {
        let owner = Arc::clone(&self.thread);
        tauri::async_runtime::spawn_blocking(move || {
            if let Ok(service) = ColorApplyService::new(owner)
                && let Ok(operations) = service.pending_operations(session)
            {
                for operation in operations {
                    resolve_color(&service, session, operation);
                }
            }
        });
    }

    fn color_task(
        &self,
        kind: ImportOperationKind,
        id: Uuid,
        status: String,
        error_code: Option<String>,
    ) -> Result<ImportTaskView, AppError> {
        Ok(ImportTaskView {
            task_id: self.uuid_text(id.to_string())?,
            operation_kind: kind,
            status,
            error_code,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            report_export: None,
            workspace_backup: None,
            video_frame: None,
            video_sample: None,
            video_tracking: None,
            staging_commit: None,
            color_preview: None,
            color_apply: None,
        })
    }

    fn color_error(&self, error: ColorPreviewError) -> AppError {
        match error {
            ColorPreviewError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::UnknownTask,
            )) => self.error("NOT_FOUND", "当前工作区中没有此颜色任务。"),
            ColorPreviewError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "同一颜色任务的参数已变化。"),
            ColorPreviewError::Workspace(error) => self.dataset_error(error),
            ColorPreviewError::InvalidBinding => self.error(
                "FAILED_PRECONDITION",
                "请核对当前图片、颜色参数和已核验候选。",
            ),
            ColorPreviewError::ResultUnconfirmed | ColorPreviewError::TaskEvidence(_) => {
                self.error("DATA_CORRUPTION", "无法核验颜色任务产物。")
            }
            ColorPreviewError::Io(_) => {
                self.error("FILE_IO_FAILED", "无法读取颜色修复所需的文件。")
            }
            _ => self.error("INTERNAL", "颜色任务无法受理或读取。"),
        }
    }
}

fn resolve_color(service: &ColorApplyService, session: Uuid, operation: Uuid) {
    let time = || {
        OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .div_euclid(1_000_000) as i64
    };
    if service.resume(session, operation, time()).is_err() {
        let _ = service.mark_resume_required(session, operation, time());
    }
}

fn core_region(region: ColorRegion) -> preview::ColorRegion {
    match region {
        ColorRegion::Rectangle { bbox_xywh } => preview::ColorRegion::Rectangle { bbox_xywh },
        ColorRegion::Polygon { points_xy } => preview::ColorRegion::Polygon { points_xy },
    }
}

fn wire_region(region: preview::ColorRegion) -> ColorRegion {
    match region {
        preview::ColorRegion::Rectangle { bbox_xywh } => ColorRegion::Rectangle { bbox_xywh },
        preview::ColorRegion::Polygon { points_xy } => ColorRegion::Polygon { points_xy },
    }
}

fn core_options(options: ColorOptions) -> preview::ColorOptions {
    preview::ColorOptions {
        method: match options.method {
            ColorMethod::HueRemap => preview::RepairColorMethod::HueRemap,
            ColorMethod::SolidFill => preview::RepairColorMethod::SolidFill,
        },
        target_rgb: options.target_rgb,
        strength: options.strength.unwrap_or(1.0),
        feather_px: options.feather_px.unwrap_or(0.0),
        source_color_gate: options.source_color_gate.map(|gate| preview::ColorGate {
            hue_min_deg: gate.hue_min_deg,
            hue_max_deg: gate.hue_max_deg,
            wraps_zero: gate.wraps_zero,
        }),
    }
}

fn wire_options(options: preview::ColorOptions) -> ColorOptions {
    ColorOptions {
        method: match options.method {
            preview::RepairColorMethod::HueRemap => ColorMethod::HueRemap,
            preview::RepairColorMethod::SolidFill => ColorMethod::SolidFill,
        },
        target_rgb: options.target_rgb,
        strength: Some(options.strength),
        feather_px: Some(options.feather_px),
        source_color_gate: options.source_color_gate.map(|gate| ColorHueGate {
            hue_min_deg: gate.hue_min_deg,
            hue_max_deg: gate.hue_max_deg,
            wraps_zero: gate.wraps_zero,
        }),
    }
}
