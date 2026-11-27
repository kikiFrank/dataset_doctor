import { invoke } from "@tauri-apps/api/core";
import type {
  AnnotationView, CocoDocument, DraftReceipt, ImportOperationRef, ReviewCancelRequest, ReviewCommitRequest,
  ReviewEditorView, ReviewOpenRequest, ReviewSaveDraftRequest, ReviewSession, ReviewTrackSelection,
  PathGrant, ReviewWriteAuthorizeRequest,
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
  trackSelections?: ReviewTrackSelection[],
): Promise<DraftReceipt> {
  const request: ReviewSaveDraftRequest = {
    context: context(workspace, crypto.randomUUID()),
    review_id: review.review_id,
    source_task_id: taskId,
    expected_review_revision: review.row_revision,
    expected_draft_revision: previous?.draft_revision ?? null,
    coco_document: document,
    ...(trackSelections === undefined ? {} : { track_selections: trackSelections }),
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

export function submitReviewCommit(
  workspace: OpenWorkspace, review: ReviewSession, draft: DraftReceipt, negative: boolean, idempotencyKey: string,
  annotationWriteGrantId?: string,
): Promise<ImportOperationRef> {
  const request: ReviewCommitRequest = {
    annotation_write_grant_id: annotationWriteGrantId,
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    review_id: review.review_id,
    expected_review_revision: draft.review_revision,
    base_image_digest: review.base_image_digest,
    base_annotation_digest: review.base_annotation_digest,
    draft_digest: draft.draft_digest,
    confirmed_negative: negative,
  };
  return invoke("review_commit", { request });
}

export function authorizeReviewWrite(workspace: OpenWorkspace, review: ReviewSession): Promise<PathGrant | null> {
  const request: ReviewWriteAuthorizeRequest = {
    context: context(workspace, crypto.randomUUID()), review_id: review.review_id,
  };
  return invoke("review_write_authorize", { request });
}
