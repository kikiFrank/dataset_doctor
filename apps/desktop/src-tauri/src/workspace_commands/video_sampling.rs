use std::sync::Arc;

use dd_infra::task_store::TaskStoreError;
use dd_infra::video_keyframes::{
    VideoAnnotationError, VideoSampleService, VideoSampleSubmission, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use dd_infra::workspace_thread::WorkspaceThreadError;
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::video::{
    DatasetSamplingMode, StagedVideoSampleView, VideoSampleSubmitRequest, VideoSampleView,
};
use tauri::State;
use uuid::Uuid;

use super::video::VideoRunner;
use super::{AppError, Commands, WorkspaceThread, run};

#[tauri::command]
pub async fn video_sample_submit(
    request: VideoSampleSubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runner: State<'_, Option<Arc<VideoRunner>>>,
) -> Result<ImportOperationRef, AppError> {
    let runner = runner.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.start_video_sample(runner, request),
    )
    .await
}

impl Commands {
    pub(super) fn start_video_sample(
        &self,
        runner: Option<Arc<VideoRunner>>,
        request: VideoSampleSubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        if request.tracking_result_ref.is_some() {
            return Err(self.error(
                "FAILED_PRECONDITION",
                "当前只能依据人工关键帧和实际视频帧采样。",
            ));
        }
        let config = request.dataset_sampling_config;
        let mode = match (config.mode, config.every_n, config.interval_ms) {
            (DatasetSamplingMode::EveryNFrames, Some(every_n), None) => {
                VideoSamplingMode::EveryNFrames { every_n }
            }
            (DatasetSamplingMode::TimeInterval, None, Some(interval_ms)) => {
                VideoSamplingMode::TimeInterval { interval_ms }
            }
            _ => return Err(self.error("INVALID_ARGUMENT", "采样间隔必须与所选模式对应。")),
        };
        let selection = VideoSamplingRequest {
            source_video_id: self.parse_uuid(request.source_video_id)?,
            target_dataset_id: self.parse_uuid(request.target_dataset_id)?,
            segment_id: self.parse_uuid(request.segment_id)?,
            mode,
            purpose: VideoSamplingPurpose::Dataset {
                include_keyframes: config.include_keyframes.unwrap_or(false),
                max_frames: config.max_frames,
            },
        };
        let service = self.video_sample_service()?;
        let (mut snapshot, job) = service
            .submit(
                session,
                VideoSampleSubmission {
                    request_id,
                    idempotency_key,
                    selection,
                },
                self.now_ms()?,
            )
            .map_err(|error| self.video_sample_error(error))?;
        if let Some(job) = job {
            match runner {
                Some(runner) => {
                    tauri::async_runtime::spawn(runner.execute_sample(service, session, job));
                }
                None => {
                    service
                        .record_worker_unavailable(session, job)
                        .map_err(|error| self.video_sample_error(error))?;
                    snapshot.status = "FAILED".into();
                }
            }
        }
        self.import_operation(
            ImportOperationKind::VideoSample,
            snapshot.task_id,
            snapshot.status,
            snapshot.accepted_at_ms,
        )
    }

    pub(super) fn video_sample_task(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let snapshot = self
            .video_sample_service()?
            .snapshot(session, task, self.now_ms()?)
            .map_err(|error| self.video_sample_error(error))?
            .ok_or_else(|| self.error("NOT_FOUND", "当前工作区中没有此任务。"))?;
        let video_sample = snapshot
            .receipt
            .map(|receipt| {
                Ok::<_, AppError>(VideoSampleView {
                    source_video_id: self.uuid_text(receipt.source_video_id.to_string())?,
                    target_dataset_id: self.uuid_text(receipt.target_dataset_id.to_string())?,
                    segment_id: self.uuid_text(receipt.segment_id.to_string())?,
                    segment_revision: receipt.segment_revision.into(),
                    sampling_config_id: self.uuid_text(receipt.sampling_config_id.to_string())?,
                    samples: receipt
                        .samples
                        .into_iter()
                        .map(|item| {
                            Ok(StagedVideoSampleView {
                                asset_id: self.uuid_text(item.asset_id.to_string())?,
                                frame_index: item.frame_index.into(),
                                already_in_dataset: item.already_in_dataset,
                            })
                        })
                        .collect::<Result<_, AppError>>()?,
                })
            })
            .transpose()?;
        Ok(ImportTaskView {
            task_id: self.uuid_text(snapshot.task_id.to_string())?,
            operation_kind: ImportOperationKind::VideoSample,
            status: snapshot.status,
            error_code: snapshot.error_code,
            video_sample,
            video_frame: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            workspace_backup: None,
        })
    }

    fn video_sample_service(&self) -> Result<VideoSampleService, AppError> {
        VideoSampleService::new(Arc::clone(&self.thread))
            .map_err(|error| self.video_sample_error(error))
    }

    fn video_sample_error(&self, error: VideoAnnotationError) -> AppError {
        match error {
            VideoAnnotationError::Storage(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "同一采样请求的参数已变化。"),
            error => self.video_annotation_error(error),
        }
    }
}
