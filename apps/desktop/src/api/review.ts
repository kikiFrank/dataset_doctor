import { invoke } from "@tauri-apps/api/core";
import type {
  AnnotationView, CocoDocument, DraftReceipt, ReviewCancelRequest, ReviewEditorView,
  ReviewOpenRequest, ReviewSaveDraftRequest, ReviewSession,
} from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function openReview(workspace: OpenWorkspace, datasetId: string, annotation: AnnotationView): Promise<ReviewEditorView> {
  const request: ReviewOpenRequest = {
    context: context(workspace, crypto.randomUUID()),
    dataset_id: datasetId,
    asset_id: annotation.asset_id,
    base_image_digest: annotation.image_digest,
    base_annotation_digest: annotation.annotation_digest,
  };
  return invoke("review_open", { request });
}

export function saveReviewDraft(
  workspace: OpenWorkspace, review: ReviewSession, taskId: string, previous: DraftReceipt | null, document: CocoDocument,
): Promise<DraftReceipt> {
  const request: ReviewSaveDraftRequest = {
    context: context(workspace, crypto.randomUUID()),
    review_id: review.review_id,
    source_task_id: taskId,
    expected_review_revision: review.row_revision,
    expected_draft_revision: previous?.draft_revision ?? null,
    coco_document: document,
  };
  return invoke("review_save_draft", { request });
}

export function cancelReview(workspace: OpenWorkspace, review: ReviewSession): Promise<ReviewSession> {
  const request: ReviewCancelRequest = {
    context: context(workspace, crypto.randomUUID()),
    review_id: review.review_id,
    expected_revision: review.row_revision,
  };
  return invoke("review_cancel", { request });
}
