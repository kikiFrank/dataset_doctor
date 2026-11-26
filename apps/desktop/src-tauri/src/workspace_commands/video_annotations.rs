use std::sync::Arc;

use dd_infra::review_validation::ReviewValidationError;
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationError, VideoAnnotationService,
};
use dd_ui_contracts::annotation::CocoDocument;
use dd_ui_contracts::video::{
    KeyframeDraftReceipt, SegmentReceipt, VideoGetKeyframeDraftRequest,
    VideoSaveKeyframeDraftRequest, VideoSaveSegmentRequest,
};
use serde::{Serialize, de::DeserializeOwned};
use tauri::State;

use super::{AppError, Commands, WorkspaceThread, run};

const CONTROL_BYTES: usize = 4 * 1024 * 1024;
const MAX_BOXES: usize = 10_000;
const MAX_KEYFRAMES: usize = 1_000;

#[tauri::command]
pub async fn video_get_keyframe_draft(
    request: VideoGetKeyframeDraftRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<CocoDocument, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.get_video_keyframe(request),
    )
    .await
}

#[tauri::command]
pub async fn video_save_keyframe_draft(
    request: VideoSaveKeyframeDraftRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<KeyframeDraftReceipt, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.save_video_keyframe(request),
    )
    .await
}

#[tauri::command]
pub async fn video_save_segment(
    request: VideoSaveSegmentRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<SegmentReceipt, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.save_video_segment(request),
    )
    .await
}

impl Commands {
    pub(super) fn get_video_keyframe(
        &self,
        request: VideoGetKeyframeDraftRequest,
    ) -> Result<CocoDocument, AppError> {
        let session = self.session(request.context)?;
        let draft = self
            .video_annotations()?
            .load_draft(
                session,
                self.parse_uuid(request.source_video_id)?,
                self.video_wire(request.annotation_ref)?,
            )
            .map_err(|error| self.video_annotation_error(error))?;
        self.video_wire(draft.coco_document)
    }

    pub(super) fn video_annotations(&self) -> Result<VideoAnnotationService, AppError> {
        VideoAnnotationService::new(Arc::clone(&self.thread))
            .map_err(|error| self.video_annotation_error(error))
    }

    pub(super) fn save_video_keyframe(
        &self,
        request: VideoSaveKeyframeDraftRequest,
    ) -> Result<KeyframeDraftReceipt, AppError> {
        if request.coco_document.annotations.len() > MAX_BOXES {
            return Err(self.error("PAYLOAD_TOO_LARGE", "单个关键帧最多保存 10000 个框。"));
        }
        self.check_video_control_size(&request)?;
        let session = self.session(request.context)?;
        let frame = self.video_wire(request.frame_ref)?;
        let coco_document = serde_json::to_vec(&request.coco_document)
            .map_err(|_| self.error("INVALID_ARGUMENT", "关键帧标注内容无效。"))?;
        let receipt = self
            .video_annotations()?
            .save(
                session,
                KeyframeDraftRequest {
                    source_video_id: self.parse_uuid(request.source_video_id)?,
                    target_dataset_id: self.parse_uuid(request.target_dataset_id)?,
                    frame_ref: frame,
                    coco_document,
                    expected_video_revision: request.expected_video_revision.into(),
                },
                self.now_ms()?,
            )
            .map_err(|error| self.video_annotation_error(error))?;
        Ok(KeyframeDraftReceipt {
            source_video_id: self.uuid_text(receipt.source_video_id.to_string())?,
            target_dataset_id: self.uuid_text(receipt.target_dataset_id.to_string())?,
            frame_ref: self.video_wire(receipt.frame_ref)?,
            annotation_ref: self.video_wire(receipt.annotation_ref)?,
            draft_digest: receipt.draft_digest,
            video_revision: receipt.video_revision.into(),
        })
    }

    pub(super) fn save_video_segment(
        &self,
        request: VideoSaveSegmentRequest,
    ) -> Result<SegmentReceipt, AppError> {
        if request.keyframes.len() > MAX_KEYFRAMES {
            return Err(self.error("PAYLOAD_TOO_LARGE", "单个片段最多保存 1000 个关键帧。"));
        }
        self.check_video_control_size(&request)?;
        let session = self.session(request.context)?;
        let receipt = self
            .video_annotations()?
            .save_segment(
                session,
                SegmentRequest {
                    source_video_id: self.parse_uuid(request.source_video_id)?,
                    target_dataset_id: self.parse_uuid(request.target_dataset_id)?,
                    segment_id: request
                        .segment_id
                        .map(|value| self.parse_uuid(value))
                        .transpose()?,
                    start_us: request.start_us.into(),
                    end_us: request.end_us.into(),
                    keyframes: self.video_wire(request.keyframes)?,
                    expected_revision: request.expected_revision.into(),
                },
                self.now_ms()?,
            )
            .map_err(|error| self.video_annotation_error(error))?;
        Ok(SegmentReceipt {
            segment_id: self.uuid_text(receipt.segment_id.to_string())?,
            revision: receipt.revision.into(),
            video_annotation_manifest_ref: self
                .video_wire(receipt.video_annotation_manifest_ref)?,
            validation_warnings: receipt.validation_warnings,
        })
    }

    fn check_video_control_size(&self, request: &impl Serialize) -> Result<(), AppError> {
        let encoded = serde_json::to_vec(request)
            .map_err(|_| self.error("INVALID_ARGUMENT", "视频工作请求无效。"))?;
        if encoded.len() > CONTROL_BYTES {
            return Err(self.error("PAYLOAD_TOO_LARGE", "视频工作请求超过 4 MiB。"));
        }
        Ok(())
    }

    // The storage and desktop types share the same JSON representation.
    pub(super) fn video_wire<T: DeserializeOwned>(
        &self,
        value: impl Serialize,
    ) -> Result<T, AppError> {
        serde_json::to_value(value)
            .and_then(serde_json::from_value)
            .map_err(|_| self.error("INTERNAL", "无法读写视频工作状态。"))
    }

    pub(super) fn video_annotation_error(&self, error: VideoAnnotationError) -> AppError {
        match error {
            VideoAnnotationError::Storage(error) => self.dataset_error(error),
            VideoAnnotationError::RevisionConflict => {
                self.error("CONFLICT", "工作状态或类别已更新，请刷新后重试。")
            }
            VideoAnnotationError::InvalidInput => {
                self.error("INVALID_ARGUMENT", "请核对帧、片段区间和目标数据集。")
            }
            VideoAnnotationError::TrackingEvidenceInvalid => {
                self.error("FAILED_PRECONDITION", "请选择当前片段已核验的追踪结果。")
            }
            VideoAnnotationError::Io(_) => self.error("FILE_IO_FAILED", "无法读写视频工作文件。"),
            VideoAnnotationError::Evidence(error) => {
                match error.downcast_ref::<ReviewValidationError>() {
                    Some(ReviewValidationError::InvalidBox { .. }) => {
                        self.error("INVALID_BBOX", "标注框超出图像范围或没有有效面积。")
                    }
                    Some(_) => self.error("INVALID_ARGUMENT", "请核对标注尺寸、类别及引用。"),
                    None => self.error("FAILED_PRECONDITION", "无法核验视频帧或来源。"),
                }
            }
        }
    }
}
