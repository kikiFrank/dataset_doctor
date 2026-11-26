use std::sync::Arc;

use dd_infra::frame_runtime::FrameService;
use dd_infra::staging::{StagingFrameSource, StagingQuery, StagingState as StoredState};
use dd_infra::task_store::format_digest;
use dd_ui_contracts::staging::{StagingItem, StagingListRequest, StagingPage, StagingState};
use dd_ui_contracts::video::VideoFrame;
use tauri::State;
use uuid::Uuid;

use super::{AppError, Commands, WorkspaceThread, run};

#[tauri::command]
pub async fn staging_list(
    request: StagingListRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<StagingPage, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.list_staging(request),
    )
    .await
}

impl Commands {
    pub(super) fn list_staging(
        &self,
        request: StagingListRequest,
    ) -> Result<StagingPage, AppError> {
        let session = self.session(request.context)?;
        let page = self
            .thread
            .list_staging(
                session,
                StagingQuery {
                    target_dataset_id: request
                        .target_dataset_id
                        .map(|id| self.parse_uuid(id))
                        .transpose()?,
                    after: request.cursor.map(|id| self.parse_uuid(id)).transpose()?,
                    status: request.status.map(stored_state),
                    page_size: request.page_size,
                },
            )
            .map_err(|error| self.dataset_error(error))?;
        let frames = if page.items.iter().any(|item| item.source_frame.is_some()) {
            Some(
                FrameService::new(Arc::clone(&self.thread))
                    .map_err(|_| self.error("FAILED_PRECONDITION", "无法初始化帧证据读取。"))?,
            )
        } else {
            None
        };
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let source_frame_ref = match (&frames, item.source_frame) {
                    (Some(frames), Some(source)) => self.staged_frame(frames, session, source)?,
                    _ => None,
                };
                Ok(StagingItem {
                    asset_id: self.uuid_text(item.asset_id.to_string())?,
                    target_dataset_id: item
                        .target_dataset_id
                        .map(|id| self.uuid_text(id.to_string()))
                        .transpose()?,
                    display_name: item.display_name,
                    state: wire_state(item.state),
                    review_state: item.review_state,
                    source_frame_ref,
                    current_report_ref: None,
                    blocking_reasons: item
                        .blocking_reasons
                        .into_iter()
                        .map(String::from)
                        .collect(),
                })
            })
            .collect::<Result<_, AppError>>()?;
        Ok(StagingPage {
            items,
            next_cursor: page
                .next_cursor
                .map(|id| self.uuid_text(id.to_string()))
                .transpose()?,
        })
    }

    fn staged_frame(
        &self,
        frames: &FrameService,
        session: Uuid,
        source: StagingFrameSource,
    ) -> Result<Option<VideoFrame>, AppError> {
        let Some(task) = source.extraction_task_id else {
            return Ok(None);
        };
        let Ok(snapshot) = frames.snapshot(session, task) else {
            return Ok(None);
        };
        let Some(resolved) = snapshot.frame else {
            return Ok(None);
        };
        let frame = resolved.frame;
        if frame.source_video_id != source.source_video_id
            || format_digest(&frame.source_digest) != source.source_digest
            || frame.stream_index != source.stream_index
            || frame.frame_index != source.frame_index
            || frame.pts_us != source.pts_us
        {
            return Ok(None);
        }
        self.frame_view(task, resolved).map(Some)
    }
}

fn wire_state(state: StoredState) -> StagingState {
    match state {
        StoredState::Offline => StagingState::Offline,
        StoredState::Conflict => StagingState::Conflict,
        StoredState::Accepting => StagingState::Accepting,
        StoredState::Checking => StagingState::Checking,
        StoredState::NeedsAnnotation => StagingState::NeedsAnnotation,
        StoredState::NeedsReview => StagingState::NeedsReview,
        StoredState::NeedsCheck => StagingState::NeedsCheck,
        StoredState::ReadyForAcceptance => StagingState::ReadyForAcceptance,
    }
}

fn stored_state(state: StagingState) -> StoredState {
    match state {
        StagingState::Offline => StoredState::Offline,
        StagingState::Conflict => StoredState::Conflict,
        StagingState::Accepting => StoredState::Accepting,
        StagingState::Checking => StoredState::Checking,
        StagingState::NeedsAnnotation => StoredState::NeedsAnnotation,
        StagingState::NeedsReview => StoredState::NeedsReview,
        StagingState::NeedsCheck => StoredState::NeedsCheck,
        StagingState::ReadyForAcceptance => StoredState::ReadyForAcceptance,
    }
}
