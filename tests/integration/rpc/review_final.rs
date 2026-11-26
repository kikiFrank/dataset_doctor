#[path = "support/review.rs"]
mod review;

use std::fs;
use std::sync::Arc;

use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::review_store::ReviewStatus;
use review::{ORIGINAL, ReviewFixture};
use rusqlite::Connection;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn final_preparation_requires_explicit_approval_and_replays_after_reopen() {
    let ReviewFixture {
        root,
        formal,
        owner,
        session,
        task,
        opened,
    } = ReviewFixture::open().await;
    let drafts = ReviewDraftService::new(Arc::clone(&owner)).unwrap();
    let final_reviews = ReviewFinalService::new(Arc::clone(&owner)).unwrap();
    let mut candidate: Value = serde_json::from_slice(ORIGINAL).unwrap();
    candidate["annotations"] = json!([]);
    candidate["categories"] = json!([]);
    let saved = drafts
        .save(
            session,
            ReviewDraftRequest {
                review_id: opened.review_id,
                source_task_id: task,
                expected_review_revision: 0,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&candidate).unwrap(),
            },
            13,
        )
        .unwrap();
    let mut request = ReviewFinalRequest {
        review_id: opened.review_id,
        expected_review_revision: saved.review_revision,
        base_image_digest: opened.base_image_digest,
        base_annotation_digest: opened.base_annotation_digest,
        draft_digest: saved.draft_digest,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        confirmed_negative: false,
    };
    assert!(final_reviews.prepare(session, request.clone(), 14).is_err());
    assert_eq!(
        owner.get_review(session, opened.review_id).unwrap().status,
        ReviewStatus::Open
    );
    request.confirmed_negative = true;
    let accepted = final_reviews.prepare(session, request.clone(), 15).unwrap();
    assert_eq!(accepted.state, FinalOperationState::Prepared);
    assert_eq!(
        owner.get_review(session, opened.review_id).unwrap().status,
        ReviewStatus::Committing
    );
    assert!(
        owner
            .cancel_review(session, opened.review_id, saved.review_revision + 1, 16)
            .is_err()
    );
    assert_eq!(fs::read(&formal).unwrap(), ORIGINAL);

    owner.close(session).unwrap();
    let connection = Connection::open(root.join("workspace.db")).unwrap();
    let (operations, finals): (i64, i64) = connection
        .query_row(
            "SELECT count(*), (SELECT count(final_review_id) FROM dataset_assets)
         FROM file_operations WHERE operation_kind='REVIEW_COMMIT' AND status='PREPARED'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((operations, finals), (1, 0));
    drop(connection);

    let selection = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(selection.grant_id).unwrap().workspace_session_id;
    request.request_id = Uuid::new_v4();
    assert_eq!(
        final_reviews
            .prepare(reopened, request.clone(), 17)
            .unwrap(),
        accepted
    );
    request.confirmed_negative = false;
    assert!(final_reviews.prepare(reopened, request, 18).is_err());
    assert_eq!(fs::read(&formal).unwrap(), ORIGINAL);
    owner.close(reopened).unwrap();
}
