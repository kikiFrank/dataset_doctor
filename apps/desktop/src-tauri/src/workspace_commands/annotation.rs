use std::sync::Arc;

use dd_infra::annotation_view::{AnnotationReadError, AnnotationReader};
use dd_infra::video_keyframes::{VideoAnnotationError, VideoAnnotationService};
use dd_infra::workspace_thread::WorkspaceThread;
use dd_ui_contracts::annotation::{
    AnnotationGetRequest, AnnotationView, StagingTrackingCandidatesView,
};
use dd_ui_contracts::workspace::AppError;
use tauri::State;

use super::{Commands, dataset::annotation_state, run};
use dd_infra::media_display::MediaDisplayService;
use dd_ui_contracts::annotation::{DisplayLocator, MediaGetLocatorRequest};
use std::time::Instant;
use tauri::http::{Method, Request, Response};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[tauri::command]
pub async fn media_get_locator(
    request: MediaGetLocatorRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
    media: State<'_, Arc<MediaDisplayService>>,
) -> Result<DisplayLocator, AppError> {
    let media = Arc::clone(media.inner());
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.media_locator(&media, request),
    )
    .await
}

pub(crate) fn media_response(
    service: &MediaDisplayService,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let granted = if request.method() == Method::GET && request.uri().query().is_none() {
        request
            .uri()
            .path()
            .strip_prefix('/')
            .and_then(|text| Uuid::parse_str(text).ok())
            .and_then(|id| service.resolve(id, Instant::now()).ok())
    } else {
        None
    };
    let (status, media_type, bytes) = match granted {
        Some(image) => (200, image.media_type, image.bytes),
        None => (403, "text/plain", Vec::new()),
    };
    Response::builder()
        .status(status)
        .header("Content-Type", media_type)
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(bytes)
        .expect("fixed media response headers")
}

#[tauri::command]
pub async fn annotation_get(
    request: AnnotationGetRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<AnnotationView, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.annotation(request),
    )
    .await
}

impl Commands {
    pub(super) fn media_locator(
        &self,
        media: &MediaDisplayService,
        request: MediaGetLocatorRequest,
    ) -> Result<DisplayLocator, AppError> {
        let session = self.session(request.context)?;
        let now = Instant::now();
        let grant = match (request.asset_id, request.artifact_id, request.owner_task_id) {
            (Some(asset), None, None) => media
                .issue(session, self.parse_uuid(asset)?, now)
                .map_err(|_| self.error("FAILED_PRECONDITION", "无法授权显示此图像。"))?,
            (None, Some(artifact), Some(task)) => {
                let task = self.parse_uuid(task)?;
                let artifact = self.parse_uuid(artifact)?;
                dd_infra::frame_runtime::FrameService::new(Arc::clone(&self.thread))
                    .and_then(|frames| frames.image_locator(media, session, task, artifact, now))
                    .or_else(|_| {
                        dd_infra::color_preview::ColorPreviewService::new(Arc::clone(&self.thread))
                            .and_then(|colors| {
                                colors.image_locator(media, session, task, artifact, now)
                            })
                    })
                    .map_err(|_| self.error("FAILED_PRECONDITION", "无法核验此图像产物。"))?
            }
            _ => return Err(self.error("INVALID_ARGUMENT", "请选择图片或明确归属的帧产物。")),
        };
        let image = media
            .resolve(grant.grant_id, now)
            .map_err(|_| self.error("FAILED_PRECONDITION", "图像已改变或无法读取。"))?;
        let locator = if cfg!(windows) {
            format!("http://dd-media.localhost/{}", grant.grant_id)
        } else {
            format!("dd-media://localhost/{}", grant.grant_id)
        };
        let expires_at = (OffsetDateTime::now_utc() + super::GRANT_LIFETIME)
            .format(&Rfc3339)
            .map_err(|_| self.error("INTERNAL", "无法生成图像显示期限。"))?;
        Ok(DisplayLocator {
            locator,
            media_type: image.media_type.into(),
            expires_at,
            resource_digest: grant.resource_digest,
        })
    }

    pub(super) fn annotation(
        &self,
        request: AnnotationGetRequest,
    ) -> Result<AnnotationView, AppError> {
        let session = self.session(request.context)?;
        let asset = self.parse_uuid(request.asset_id)?;
        let reader = AnnotationReader::new(Arc::clone(&self.thread))
            .map_err(|_| self.error("INTERNAL", "无法初始化标注读取器。"))?;
        let current = reader.read(session, asset).map_err(|error| match error {
            AnnotationReadError::Storage(failure) => self.dataset_error(failure),
            AnnotationReadError::Io(_) => self.error(
                "FAILED_PRECONDITION",
                "无法读取图片或标注文件，请检查资源是否在线。",
            ),
            AnnotationReadError::Contract(_) => {
                self.error("INVALID_ARGUMENT", "标注文件不符合单图 COCO 契约。")
            }
            _ => self.error("INTERNAL", "无法读取当前标注。"),
        })?;
        let active_review = current
            .active_review
            .map(|id| {
                let review = self
                    .thread
                    .get_review(session, id)
                    .map_err(|error| self.review_error(error))?;
                self.review_view(review)
            })
            .transpose()?;
        let tracking_candidates = if current.annotation_digest.is_none() {
            let work = VideoAnnotationService::new(Arc::clone(&self.thread))
                .map_err(|error| self.video_annotation_error(error))?;
            match work.staging_tracking_candidates(session, current.dataset_id, asset) {
                Ok(Some(candidates)) => Some(StagingTrackingCandidatesView::Available {
                    tracks_ref: self.wire_artifact(candidates.reference)?,
                    frame_index: candidates.frame_index.into(),
                    observations: candidates
                        .observations
                        .into_iter()
                        .map(|observation| self.tracking_observation(observation))
                        .collect::<Result<_, AppError>>()?,
                }),
                Ok(None) => None,
                Err(
                    VideoAnnotationError::TrackingEvidenceInvalid | VideoAnnotationError::Io(_),
                ) => Some(StagingTrackingCandidatesView::Unavailable),
                Err(error) => return Err(self.video_annotation_error(error)),
            }
        } else {
            None
        };
        Ok(AnnotationView {
            asset_id: self.uuid_text(current.asset_id.to_string())?,
            image_digest: current.image_digest,
            annotation_digest: current.annotation_digest,
            formal_coco: current
                .formal_coco
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| self.error("INVALID_ARGUMENT", "标注数值超出支持范围。"))?,
            annotation_state: annotation_state(current.annotation_state),
            active_review,
            tracking_candidates,
        })
    }
}
