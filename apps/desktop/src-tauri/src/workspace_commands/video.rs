use std::{path::PathBuf, sync::Arc};

use dd_infra::video_query::{VideoSourceAvailability, registered_video_detail};
use dd_infra::video_registration::{
    VideoImport, VideoImportError, VideoImportReceipt, VideoRegistry,
};
use dd_infra::video_runtime::VideoProbeRuntime;
use dd_infra::video_runtime::VideoRuntimeError;
use dd_infra::workspace_thread::WorkspaceThread;
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportTaskView};
use dd_ui_contracts::video::{
    VideoAvailability, VideoDetail, VideoGetRequest, VideoImportRequest, VideoImportView,
    VideoStreamView,
};
use dd_ui_contracts::workspace::{AppError, CommandContext, PathGrant};
use tauri::{State, async_runtime};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::{Commands, GRANT_LIFETIME, configured_worker_script, run};

pub(crate) struct VideoRunner {
    interpreter: PathBuf,
    entrypoint: PathBuf,
    slot: async_runtime::Mutex<()>,
}

impl VideoRunner {
    pub(crate) fn new(interpreter: PathBuf, entrypoint: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            interpreter,
            entrypoint,
            slot: async_runtime::Mutex::new(()),
        })
    }

    pub(super) async fn execute_frame(
        self: Arc<Self>,
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        job: dd_infra::frame_runtime::FrameResolveJob,
    ) {
        use dd_infra::frame_runtime::FrameResolveRuntime;
        let task = job.task_id();
        let _slot = self.slot.lock().await;
        let owner = Arc::clone(&workspace);
        let state = async_runtime::spawn_blocking(move || {
            FrameResolveRuntime::task_outcome(&owner, session, task)
        })
        .await;
        if !matches!(state, Ok(Ok(outcome)) if outcome.status == "QUEUED") {
            return;
        }
        let started = FrameResolveRuntime::launch(
            Arc::clone(&workspace),
            session,
            self.interpreter.clone(),
            self.entrypoint.clone(),
        )
        .await;
        let startup_failed = match started {
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
        let _ = async_runtime::spawn_blocking(move || {
            FrameResolveRuntime::record_unavailable(&workspace, session, task, startup_failed)
        })
        .await;
    }

    pub(crate) fn configured() -> Option<Arc<Self>> {
        let (interpreter, entrypoint) = configured_worker_script()?;
        Some(Self::new(interpreter, entrypoint))
    }

    async fn execute(
        self: Arc<Self>,
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        receipt: VideoImportReceipt,
    ) {
        let _slot = self.slot.lock().await;
        let status = {
            let workspace = Arc::clone(&workspace);
            async_runtime::spawn_blocking(move || {
                VideoProbeRuntime::task_outcome(&workspace, session, receipt.task_id)
            })
            .await
        };
        if !matches!(status, Ok(Ok(outcome)) if outcome.status == "QUEUED") {
            return;
        }
        let worker = VideoProbeRuntime::launch(
            Arc::clone(&workspace),
            session,
            self.interpreter.clone(),
            self.entrypoint.clone(),
        )
        .await;
        let startup_failed = match worker {
            Ok(mut worker) => {
                if worker.run(receipt).await.is_ok() {
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
        let _ = async_runtime::spawn_blocking(move || {
            VideoProbeRuntime::record_unavailable(
                &workspace,
                session,
                receipt.task_id,
                startup_failed,
            )
        })
        .await;
    }
}

#[tauri::command]
pub async fn video_import(
    request: VideoImportRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runner: State<'_, Option<Arc<VideoRunner>>>,
) -> Result<VideoImportView, AppError> {
    let runner = runner.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| {
            let runner = runner
                .ok_or_else(|| commands.error("FAILED_PRECONDITION", "视频 Worker 尚未配置。"))?;
            commands.start_video_import(runner, request)
        },
    )
    .await
}

impl Commands {
    pub(super) fn resume_pending_video_probes(
        &self,
        runner: Arc<VideoRunner>,
        session: Uuid,
    ) -> Result<(), AppError> {
        let registry = VideoRegistry::new(Arc::clone(&self.thread))
            .map_err(|error| self.video_import_error(error))?;
        let pending = registry
            .pending_probes(session, self.now_ms()?)
            .map_err(|error| self.video_import_error(error))?;
        for receipt in pending {
            async_runtime::spawn(Arc::clone(&runner).execute(
                Arc::clone(&self.thread),
                session,
                receipt,
            ));
        }
        Ok(())
    }

    pub(super) fn video_file(
        &self,
        context: CommandContext,
        path: PathBuf,
    ) -> Result<PathGrant, AppError> {
        let session = self.session(context)?;
        let grant = self
            .thread
            .grant_video_file(session, path.clone())
            .map_err(|error| self.map_error(error))?;
        let expires_at = (OffsetDateTime::now_utc() + GRANT_LIFETIME)
            .format(&Rfc3339)
            .map_err(|_| self.error("INTERNAL", "无法生成视频文件授权。"))?;
        Ok(PathGrant {
            grant_id: self.uuid_text(grant.grant_id.to_string())?,
            display_path: path.to_string_lossy().into_owned(),
            allowed_operations: vec!["VIDEO_IMPORT".into()],
            expires_at,
        })
    }

    pub(super) fn start_video_import(
        &self,
        runner: Arc<VideoRunner>,
        request: VideoImportRequest,
    ) -> Result<VideoImportView, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let registry = VideoRegistry::new(Arc::clone(&self.thread))
            .map_err(|error| self.video_import_error(error))?;
        let receipt = registry
            .admit(
                session,
                VideoImport {
                    request_id,
                    idempotency_key,
                    file_grant_id: self.parse_uuid(request.file_grant_id)?,
                    display_name: request.display_name,
                    stream_index: request.stream_index,
                },
                self.now_ms()?,
            )
            .map_err(|error| self.video_import_error(error))?;
        let status = VideoProbeRuntime::task_outcome(&self.thread, session, receipt.task_id)
            .map_err(|error| self.dataset_error(error))?
            .status;
        if status == "QUEUED" {
            async_runtime::spawn(runner.execute(Arc::clone(&self.thread), session, receipt));
        }
        Ok(VideoImportView {
            source_video_id: self.uuid_text(receipt.video_id.to_string())?,
            operation: self.import_operation(
                ImportOperationKind::VideoProbe,
                receipt.task_id,
                status,
                receipt.accepted_at_ms,
            )?,
        })
    }

    pub(super) fn video_task(&self, session: Uuid, task: Uuid) -> Result<ImportTaskView, AppError> {
        let outcome = VideoProbeRuntime::task_outcome(&self.thread, session, task)
            .map_err(|error| self.dataset_error(error))?;
        Ok(ImportTaskView {
            task_id: self.uuid_text(task.to_string())?,
            operation_kind: ImportOperationKind::VideoProbe,
            status: outcome.status,
            error_code: outcome.error_code,
            video_frame: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            workspace_backup: None,
        })
    }

    fn video_import_error(&self, error: VideoImportError) -> AppError {
        match error {
            VideoImportError::InvalidRequest => {
                self.error("INVALID_ARGUMENT", "视频导入请求无效。")
            }
            VideoImportError::IdempotencyConflict => {
                self.error("CONFLICT", "同一请求的参数已变化。")
            }
            VideoImportError::SourceChanged => {
                self.error("INPUT_CHANGED", "已登记的视频内容发生变化。")
            }
            VideoImportError::Storage(error) => self.dataset_error(error),
            VideoImportError::File(_) => self.error("FILE_IO_FAILED", "无法读取视频文件。"),
            _ => self.error("INTERNAL", "未能登记视频。"),
        }
    }
}

#[tauri::command]
pub async fn video_get(
    request: VideoGetRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<VideoDetail, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.video_detail(request),
    )
    .await
}

impl Commands {
    pub(super) fn video_detail(&self, request: VideoGetRequest) -> Result<VideoDetail, AppError> {
        let session = self.session(request.context)?;
        let source = self.parse_uuid(request.source_video_id)?;
        let detail =
            registered_video_detail(&self.thread, session, source).map_err(
                |error| match error {
                    VideoRuntimeError::Workspace(failure) => self.dataset_error(failure),
                    VideoRuntimeError::StaleWorkspace => {
                        self.error("STALE_WORKSPACE_SESSION", "工作区会话已更新。")
                    }
                    _ => self.error("FAILED_PRECONDITION", "无法核验视频探测结果，请重新探测。"),
                },
            )?;
        let (duration, variable_frame_rate, streams) = match detail.metadata {
            Some(metadata) => (
                metadata.duration_ms.map(Into::into),
                metadata.variable_frame_rate,
                metadata
                    .streams
                    .into_iter()
                    .map(|stream| VideoStreamView {
                        stream_index: stream.stream_index,
                        codec: stream.codec,
                        width: stream.width,
                        height: stream.height,
                        duration_ms: stream.duration_ms.map(Into::into),
                    })
                    .collect(),
            ),
            None => (None, None, Vec::new()),
        };
        Ok(VideoDetail {
            source_video_id: self.uuid_text(detail.video_id.to_string())?,
            display_name: detail.display_name,
            row_revision: detail.revision.into(),
            source_digest: detail.source_digest,
            source_availability: match detail.source_availability {
                VideoSourceAvailability::Online => VideoAvailability::Online,
                VideoSourceAvailability::Offline => VideoAvailability::Offline,
                VideoSourceAvailability::Changed => VideoAvailability::Changed,
            },
            probe_status: detail.probe_status,
            duration_ms: duration,
            streams,
            rotation_ccw: detail.rotation_ccw,
            variable_frame_rate,
        })
    }
}
