use std::sync::Arc;

use dd_infra::review_draft::{self, ReviewDraftError, ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_store::{self, ReviewBaseline, ReviewStatus, ReviewStoreError};
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::annotation::{
    AnnotationGetRequest, CocoCategory, DraftReceipt, ReviewCancelRequest, ReviewDraftView,
    ReviewEditorView, ReviewOpenRequest, ReviewSaveDraftRequest, ReviewSession,
    ReviewTrackSelection,
};
use dd_ui_contracts::workspace::AppError;
use tauri::State;

use super::{Commands, run};

#[tauri::command]
pub async fn review_open(
    request: ReviewOpenRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ReviewEditorView, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.open_review(request),
    )
    .await
}

#[tauri::command]
pub async fn review_save_draft(
    request: ReviewSaveDraftRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<DraftReceipt, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.save_review_draft(request),
    )
    .await
}

#[tauri::command]
pub async fn review_cancel(
    request: ReviewCancelRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ReviewSession, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.cancel_review(request),
    )
    .await
}

impl Commands {
    pub(super) fn open_review(
        &self,
        request: ReviewOpenRequest,
    ) -> Result<ReviewEditorView, AppError> {
        let session = self.session(request.context.clone())?;
        let dataset = self.parse_uuid(request.dataset_id)?;
        let asset = self.parse_uuid(request.asset_id.clone())?;
        let current = self.annotation(AnnotationGetRequest {
            context: request.context,
            asset_id: request.asset_id,
        })?;
        if current.image_digest != request.base_image_digest
            || current.annotation_digest != request.base_annotation_digest
        {
            return Err(self.error("CONFLICT", "样本已变化，请刷新样本。"));
        }
        let drafts = self.drafts()?;
        let review = self
            .thread
            .open_review_checked(
                session,
                dataset,
                asset,
                ReviewBaseline {
                    image_digest: request.base_image_digest,
                    annotation_digest: request.base_annotation_digest,
                },
                self.now_ms()?,
            )
            .map_err(|error| self.review_error(error))?;
        let editor = drafts
            .editor_state(session, review.review_id)
            .map_err(|error| self.draft_error(error))?;
        let draft = editor
            .draft
            .map(|draft| {
                Ok::<_, AppError>(ReviewDraftView {
                    receipt: self.draft_receipt(draft.receipt)?,
                    source_task_id: self.uuid_text(draft.source_task_id.to_string())?,
                    coco_document: serde_json::from_value(draft.coco_document)
                        .map_err(|_| self.error("INTERNAL", "审核草稿数据无效。"))?,
                    track_selections: if draft.track_bindings.is_empty() {
                        None
                    } else {
                        Some(
                            draft
                                .track_bindings
                                .into_iter()
                                .map(|binding| {
                                    Ok(ReviewTrackSelection {
                                        local_annotation_id: binding.local_annotation_id,
                                        track_id: self.uuid_text(binding.track_id.to_string())?,
                                    })
                                })
                                .collect::<Result<_, AppError>>()?,
                        )
                    },
                })
            })
            .transpose()?;
        Ok(ReviewEditorView {
            review: self.review_view(editor.review)?,
            categories: editor
                .categories
                .into_iter()
                .map(|category| CocoCategory {
                    id: category.id,
                    name: category.name,
                })
                .collect(),
            draft,
        })
    }

    pub(super) fn save_review_draft(
        &self,
        request: ReviewSaveDraftRequest,
    ) -> Result<DraftReceipt, AppError> {
        let session = self.session(request.context)?;
        let drafts = self.drafts()?;
        let draft = ReviewDraftRequest {
            review_id: self.parse_uuid(request.review_id)?,
            source_task_id: self.parse_uuid(request.source_task_id)?,
            expected_review_revision: request.expected_review_revision.into(),
            expected_draft_revision: request.expected_draft_revision.map(u64::from),
            coco_document: serde_json::to_vec(&request.coco_document)
                .map_err(|_| self.error("INVALID_ARGUMENT", "草稿坐标或标注格式无效。"))?,
        };
        let now = self.now_ms()?;
        let receipt = if let Some(selections) = request.track_selections {
            if selections.len() > request.coco_document.annotations.len() {
                return Err(self.error("INVALID_ARGUMENT", "请核对轨迹和草稿标注的关联。"));
            }
            let selected = selections
                .into_iter()
                .map(|choice| {
                    Ok(review_draft::ReviewTrackSelection {
                        local_annotation_id: choice.local_annotation_id,
                        track_id: self.parse_uuid(choice.track_id)?,
                    })
                })
                .collect::<Result<Vec<_>, AppError>>()?;
            drafts.save_with_tracks(session, draft, &selected, now)
        } else {
            drafts.save(session, draft, now)
        }
        .map_err(|error| self.draft_error(error))?;
        self.draft_receipt(receipt)
    }

    pub(super) fn cancel_review(
        &self,
        request: ReviewCancelRequest,
    ) -> Result<ReviewSession, AppError> {
        let session = self.session(request.context)?;
        let review = self
            .thread
            .cancel_review(
                session,
                self.parse_uuid(request.review_id)?,
                request.expected_revision.into(),
                self.now_ms()?,
            )
            .map_err(|error| self.review_error(error))?;
        self.review_view(review)
    }

    fn drafts(&self) -> Result<ReviewDraftService, AppError> {
        ReviewDraftService::new(Arc::clone(&self.thread)).map_err(|error| self.draft_error(error))
    }

    fn draft_receipt(&self, receipt: review_draft::DraftReceipt) -> Result<DraftReceipt, AppError> {
        Ok(DraftReceipt {
            review_id: self.uuid_text(receipt.review_id.to_string())?,
            draft_revision: receipt.draft_revision.into(),
            draft_digest: receipt.draft_digest,
            review_revision: receipt.review_revision.into(),
        })
    }

    pub(super) fn review_view(
        &self,
        review: review_store::ReviewSession,
    ) -> Result<ReviewSession, AppError> {
        let status = match review.status {
            ReviewStatus::Open => "OPEN",
            ReviewStatus::Committing => "COMMITTING",
            ReviewStatus::Confirmed => "CONFIRMED",
            ReviewStatus::Cancelled => "CANCELLED",
            ReviewStatus::Conflict => "CONFLICT",
            ReviewStatus::Stale => "STALE",
        };
        Ok(ReviewSession {
            review_id: self.uuid_text(review.review_id.to_string())?,
            asset_id: self.uuid_text(review.asset_id.to_string())?,
            status: status.into(),
            row_revision: review.row_revision.into(),
            base_image_digest: review.base_image_digest,
            base_annotation_digest: review.base_annotation_digest,
            base_label_schema_digest: review.base_label_schema_digest,
        })
    }

    pub(super) fn review_error(&self, error: WorkspaceThreadError) -> AppError {
        match error {
            WorkspaceThreadError::Review(ReviewStoreError::RevisionConflict) => {
                self.error("CONFLICT", "样本或草稿已变化，请重新加载。")
            }
            WorkspaceThreadError::Review(ReviewStoreError::InvalidState) => {
                self.error("FAILED_PRECONDITION", "当前审核状态不允许此操作。")
            }
            WorkspaceThreadError::Review(ReviewStoreError::UnknownReview) => {
                self.error("NOT_FOUND", "当前工作区中没有此审核。")
            }
            WorkspaceThreadError::Review(ReviewStoreError::Dataset(error)) => {
                self.dataset_error(WorkspaceThreadError::Dataset(error))
            }
            other => self.dataset_error(other),
        }
    }

    fn draft_error(&self, error: ReviewDraftError) -> AppError {
        match error {
            ReviewDraftError::Storage(error) => self.review_error(error),
            ReviewDraftError::RevisionConflict => {
                self.error("CONFLICT", "草稿已变化，请重新加载。")
            }
            ReviewDraftError::Evidence(_) => self.error(
                "FAILED_PRECONDITION",
                "检查结果已过期或与此样本不匹配，请重新检查。",
            ),
            ReviewDraftError::Validation(_) => {
                self.error("INVALID_ARGUMENT", "请检查标注框是否越界、类别是否有效。")
            }
            ReviewDraftError::InvalidTrackingSelection => self.error(
                "FAILED_PRECONDITION",
                "无法核验所选追踪候选与当前帧和类别的关联。",
            ),
            _ => self.error("INTERNAL", "无法读取或保存审核草稿。"),
        }
    }
}
