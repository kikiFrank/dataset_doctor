use std::sync::Arc;

use dd_infra::task_store::TaskStoreError;
use dd_infra::video_keyframes::{VideoSamplingPurpose, VideoSamplingRequest};
use dd_infra::video_tracking_runtime::{
    TRANSLATION_PROFILE_ID, TrackingError, VideoTrackBundle, VideoTrackingRuntime,
    VideoTrackingService, VideoTrackingSubmission,
};
use dd_ui_contracts::DecimalU64;
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::video::{
    TrackObservationView, TrackTailCheckView, TrackUncertainRangeView, VideoTrackSubmitRequest,
    VideoTrackingView,
};
use tauri::State;
use uuid::Uuid;

use super::video::VideoRunner;
use super::{AppError, Commands, WorkspaceThread, WorkspaceThreadError, run};

#[tauri::command]
pub async fn video_track_submit(
    request: VideoTrackSubmitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    runner: State<'_, Option<Arc<VideoRunner>>>,
) -> Result<ImportOperationRef, AppError> {
    let runner = runner.inner().clone();
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.start_video_tracking(runner, request),
    )
    .await
}

impl Commands {
    pub(super) fn start_video_tracking(
        &self,
        runner: Option<Arc<VideoRunner>>,
        request: VideoTrackSubmitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let config = request.tracking_sampling_config;
        let selection = VideoSamplingRequest {
            source_video_id: self.parse_uuid(request.source_video_id)?,
            target_dataset_id: self.parse_uuid(request.target_dataset_id)?,
            segment_id: self.parse_uuid(request.segment_id)?,
            mode: self.video_sampling_mode(config.mode, config.every_n, config.interval_ms)?,
            purpose: VideoSamplingPurpose::Tracking {
                supports_time_interval: true,
            },
        };
        let profile = request
            .tracker_profile_id
            .map(|profile| self.parse_uuid(profile))
            .transpose()?
            .unwrap_or(TRANSLATION_PROFILE_ID);
        let service = self.video_tracking_service()?;
        let (mut snapshot, job) = service
            .submit(
                session,
                VideoTrackingSubmission {
                    request_id,
                    idempotency_key,
                    tracker_profile_id: profile,
                    selection,
                },
                self.now_ms()?,
            )
            .map_err(|error| self.video_tracking_error(error))?;
        if let Some(job) = job {
            match runner {
                Some(runner) => {
                    tauri::async_runtime::spawn(runner.execute_tracking(
                        Arc::clone(&self.thread),
                        session,
                        job,
                    ));
                }
                None => {
                    VideoTrackingRuntime::record_unavailable(
                        &self.thread,
                        session,
                        job.task_id(),
                        true,
                    )
                    .map_err(|error| self.video_tracking_error(error))?;
                    snapshot.status = "FAILED".into();
                }
            }
        }
        self.import_operation(
            ImportOperationKind::VideoTrack,
            snapshot.task_id,
            snapshot.status,
            snapshot.accepted_at_ms,
        )
    }

    pub(super) fn video_tracking_task(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let snapshot = self
            .video_tracking_service()?
            .snapshot(session, task)
            .map_err(|error| self.video_tracking_error(error))?;
        let video_tracking = snapshot
            .bundle
            .map(|bundle| self.video_tracking_view(bundle))
            .transpose()?;
        Ok(ImportTaskView {
            color_preview: None,
            color_apply: None,
            task_id: self.uuid_text(task.to_string())?,
            operation_kind: ImportOperationKind::VideoTrack,
            status: snapshot.status,
            error_code: snapshot.error_code,
            video_tracking,
            video_sample: None,
            video_frame: None,
            staging_commit: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            report_export: None,
            workspace_backup: None,
        })
    }

    fn video_tracking_view(&self, bundle: VideoTrackBundle) -> Result<VideoTrackingView, AppError> {
        let invalid = || self.error("DATA_CORRUPTION", "无法核验已保存的追踪结果。");
        let data = bundle.data;
        let observations = data
            .observations
            .into_iter()
            .map(|observation| self.tracking_observation(observation))
            .collect::<Result<_, AppError>>()?;
        let uncertain_ranges = data
            .uncertain_ranges
            .into_iter()
            .map(|span| {
                Ok(TrackUncertainRangeView {
                    track_id: self.uuid_text(span.track_id.to_string())?,
                    first_frame_index: DecimalU64::try_from(span.first_frame_index)
                        .map_err(|_| invalid())?,
                    last_frame_index: DecimalU64::try_from(span.last_frame_index)
                        .map_err(|_| invalid())?,
                    reason: span.reason,
                })
            })
            .collect::<Result<_, AppError>>()?;
        let tail_checks = data
            .tail_checks
            .into_iter()
            .map(|check| {
                Ok(TrackTailCheckView {
                    track_id: self.uuid_text(check.track_id.to_string())?,
                    status: check.status,
                })
            })
            .collect::<Result<_, AppError>>()?;
        Ok(VideoTrackingView {
            target_dataset_id: self.uuid_text(bundle.target_dataset_id.to_string())?,
            label_schema_digest: bundle.label_schema_digest,
            segment_revision: bundle.segment_revision.into(),
            source_video_id: self.uuid_text(data.source_video_id.to_string())?,
            source_digest: data.source_digest,
            stream_index: data.stream_index,
            segment_id: self.uuid_text(data.segment_id.to_string())?,
            tracker_profile_id: self.uuid_text(data.tracker_profile_id.to_string())?,
            input_fingerprint: data.input_fingerprint,
            algorithm: data.algorithm,
            processed_frame_count: u32::try_from(data.processed_frame_count)
                .map_err(|_| invalid())?,
            tracks_ref: self.wire_artifact(bundle.reference)?,
            observations,
            uncertain_ranges,
            tail_checks,
        })
    }

    fn video_tracking_service(&self) -> Result<VideoTrackingService, AppError> {
        VideoTrackingService::new(Arc::clone(&self.thread))
            .map_err(|error| self.video_tracking_error(error))
    }

    pub(super) fn tracking_observation(
        &self,
        observation: dd_infra::video_tracking_runtime::TrackObservation,
    ) -> Result<TrackObservationView, AppError> {
        let invalid = || self.error("DATA_CORRUPTION", "无法核验已保存的追踪结果。");
        Ok(TrackObservationView {
            frame_index: DecimalU64::try_from(observation.frame_index).map_err(|_| invalid())?,
            track_id: self.uuid_text(observation.track_id.to_string())?,
            category_id: DecimalU64::try_from(observation.category_id).map_err(|_| invalid())?,
            bbox_xywh: observation.bbox_xywh,
            source: observation.source,
            mean_absolute_error: observation.mean_absolute_error,
        })
    }

    fn video_tracking_error(&self, error: TrackingError) -> AppError {
        match error {
            TrackingError::Workspace(WorkspaceThreadError::Task(TaskStoreError::UnknownTask)) => {
                self.error("NOT_FOUND", "当前工作区中没有此追踪任务。")
            }
            TrackingError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "此追踪请求的片段或采样参数已变化，请重新发起。"),
            TrackingError::Workspace(error) => self.dataset_error(error),
            TrackingError::StaleWorkspace => {
                self.error("STALE_WORKSPACE_SESSION", "工作区会话已更新。")
            }
            TrackingError::InvalidBinding => {
                self.error("INVALID_ARGUMENT", "请核对追踪方式、关键帧和片段范围。")
            }
            _ => self.error(
                "FAILED_PRECONDITION",
                "无法核验追踪输入或结果，请核对视频与目标数据集。",
            ),
        }
    }
}
