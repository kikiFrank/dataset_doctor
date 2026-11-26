use std::sync::Arc;

use dd_infra::frame_runtime::SeekPolicy;
use dd_infra::frame_runtime::{
    FrameResolveError, FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget,
};
use dd_infra::task_store::{TaskStoreError, format_digest};
use dd_infra::video_index::ResolvedVideoFrame;
use dd_infra::workspace_thread::WorkspaceThreadError;
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportTaskView};
use dd_ui_contracts::video::{
    FrameResolution, VideoFrame, VideoFrameTarget, VideoResolveFrameRequest, VideoSeekPolicy,
};
use dd_ui_contracts::{ArtifactOwner, ArtifactReference};
use tauri::State;

use super::video::VideoRunner;
use super::{AppError, Commands, WorkspaceThread, run};

#[tauri::command]
pub async fn video_resolve_frame(
    request: VideoResolveFrameRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runner: State<'_, Option<Arc<VideoRunner>>>,
) -> Result<FrameResolution, AppError> {
    let runner = runner.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.resolve_video_frame(runner, request),
    )
    .await
}

impl Commands {
    pub(super) fn resolve_video_frame(
        &self,
        runner: Option<Arc<VideoRunner>>,
        request: VideoResolveFrameRequest,
    ) -> Result<FrameResolution, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let target = match request.target {
            VideoFrameTarget::Index { frame_index } => FrameTarget::Index(u64::from(frame_index)),
            VideoFrameTarget::Timestamp { timestamp_ms } => {
                FrameTarget::TimestampMs(u64::from(timestamp_ms))
            }
        };
        let policy = match request.seek_policy {
            VideoSeekPolicy::Nearest => SeekPolicy::Nearest,
            VideoSeekPolicy::AtOrAfter => SeekPolicy::AtOrAfter,
        };
        let service =
            FrameService::new(Arc::clone(&self.thread)).map_err(|error| self.frame_error(error))?;
        let (mut task, job) = service
            .submit(
                session,
                FrameSubmission {
                    request_id,
                    idempotency_key,
                    video_id: self.parse_uuid(request.source_video_id)?,
                    target,
                    policy,
                },
                self.now_ms()?,
            )
            .map_err(|error| self.frame_error(error))?;
        if let Some(frame) = task.frame.take() {
            return Ok(FrameResolution::Ready {
                frame: self.frame_view(task.task_id, frame)?,
            });
        }
        if let Some(job) = job {
            if let Some(runner) = runner {
                tauri::async_runtime::spawn(runner.execute_frame(
                    Arc::clone(&self.thread),
                    session,
                    job,
                ));
            } else {
                FrameResolveRuntime::record_unavailable(&self.thread, session, task.task_id, true)
                    .map_err(|error| self.frame_error(error))?;
                task = service
                    .snapshot(session, task.task_id)
                    .map_err(|error| self.frame_error(error))?;
            }
        }
        Ok(FrameResolution::Accepted {
            operation: self.import_operation(
                ImportOperationKind::FrameResolve,
                task.task_id,
                task.status,
                task.accepted_at_ms,
            )?,
        })
    }

    pub(super) fn frame_task(
        &self,
        session: uuid::Uuid,
        task: uuid::Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let snapshot = FrameService::new(Arc::clone(&self.thread))
            .and_then(|service| service.snapshot(session, task))
            .map_err(|error| self.frame_error(error))?;
        Ok(ImportTaskView {
            staging_commit: None,
            video_tracking: None,
            task_id: self.uuid_text(task.to_string())?,
            operation_kind: ImportOperationKind::FrameResolve,
            status: snapshot.status,
            error_code: snapshot.error_code,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            workspace_backup: None,
            video_sample: None,
            video_frame: snapshot
                .frame
                .map(|frame| self.frame_view(task, frame))
                .transpose()?,
        })
    }

    pub(super) fn frame_view(
        &self,
        task: uuid::Uuid,
        resolved: ResolvedVideoFrame,
    ) -> Result<VideoFrame, AppError> {
        let frame = resolved.frame;
        let image = resolved.reference;
        Ok(VideoFrame {
            source_video_id: self.uuid_text(frame.source_video_id.to_string())?,
            source_digest: format_digest(&frame.source_digest),
            stream_index: frame.stream_index,
            frame_index: frame.frame_index.into(),
            pts_us: frame.pts_us.map(Into::into),
            timestamp_ms: frame
                .pts_us
                .and_then(|value| u64::try_from(value / 1000).ok())
                .map(Into::into),
            width: frame.width,
            height: frame.height,
            image_ref: ArtifactReference {
                artifact_id: self.uuid_text(image.artifact_id)?,
                owner: ArtifactOwner::Task(self.uuid_text(task.to_string())?),
                kind: image.kind,
                media_type: image.media_type,
                sha256: image.sha256,
                size_bytes: image.size_bytes.into(),
                schema_id: image.schema_id,
            },
        })
    }

    fn frame_error(&self, error: FrameResolveError) -> AppError {
        match error {
            FrameResolveError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "此帧请求的内容已变化，请重新发起请求。"),
            FrameResolveError::Workspace(failure) => self.dataset_error(failure),
            FrameResolveError::StaleWorkspace => {
                self.error("STALE_WORKSPACE_SESSION", "工作区会话已更新。")
            }
            FrameResolveError::InvalidBinding => {
                self.error("FAILED_PRECONDITION", "请核对视频来源、帧范围和探测状态。")
            }
            _ => self.error("FAILED_PRECONDITION", "无法核验或解析视频帧。"),
        }
    }
}
