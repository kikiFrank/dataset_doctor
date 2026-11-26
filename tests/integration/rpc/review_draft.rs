#[path = "support/review.rs"]
mod review;

use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use review::{ORIGINAL, ReviewFixture};
use rusqlite::Connection;
use serde_json::{Value, json};

#[tokio::test]
async fn reviewer_can_save_replace_and_restore_validated_draft_without_touching_formal_json() {
    let ReviewFixture {
        root,
        formal,
        owner,
        session,
        task,
        opened,
    } = ReviewFixture::open().await;
    let drafts = Arc::new(ReviewDraftService::new(Arc::clone(&owner)).unwrap());
    assert!(drafts.read(session, opened.review_id).unwrap().is_none());
    let mut document: Value = serde_json::from_slice(ORIGINAL).unwrap();
    // Registration assigns the Dataset's category ID by exact name.
    document["categories"][0]["id"] = json!(1);
    document["annotations"][0]["category_id"] = json!(1);
    let mut request = ReviewDraftRequest {
        review_id: opened.review_id,
        source_task_id: task,
        expected_review_revision: 0,
        expected_draft_revision: None,
        coco_document: serde_json::to_vec(&document).unwrap(),
    };
    assert!(drafts.save(session, request.clone(), 13).is_err());
    assert!(drafts.read(session, opened.review_id).unwrap().is_none());
    assert_eq!(
        owner
            .get_review(session, opened.review_id)
            .unwrap()
            .row_revision,
        0
    );
    document["annotations"][0]["bbox"] = json!([0, 0, 2, 1]);
    request.coco_document = serde_json::to_vec(&document).unwrap();
    let saved = drafts.save(session, request.clone(), 14).unwrap();
    assert_eq!((saved.draft_revision, saved.review_revision), (1, 1));
    assert_eq!(saved.draft_digest.len(), 64);
    assert_eq!(
        drafts
            .read(session, opened.review_id)
            .unwrap()
            .unwrap()
            .coco_document["annotations"][0]["area"],
        2.0
    );
    assert_eq!(fs::read(&formal).unwrap(), ORIGINAL);
    assert!(drafts.save(session, request.clone(), 15).is_err());

    request.expected_review_revision = saved.review_revision;
    request.expected_draft_revision = Some(saved.draft_revision);
    document["annotations"] = json!([]);
    request.coco_document = serde_json::to_vec(&document).unwrap();
    let ready = Arc::new(Barrier::new(2));
    let attempts: Vec<_> = [16, 17]
        .into_iter()
        .map(|now| {
            let ready = Arc::clone(&ready);
            let drafts = Arc::clone(&drafts);
            let request = request.clone();
            thread::spawn(move || {
                ready.wait();
                drafts.save(session, request, now)
            })
        })
        .collect();
    let outcomes: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect();
    let mut successes = outcomes.into_iter().filter_map(Result::ok);
    let current = successes.next().unwrap();
    assert_eq!((current.draft_revision, current.review_revision), (2, 2));
    assert!(successes.next().is_none());
    assert_ne!(current.draft_digest, saved.draft_digest);

    owner.close(session).unwrap();
    let connection = Connection::open(root.join("workspace.db")).unwrap();
    let current_path: String = connection
        .query_row(
            "SELECT review_manifest_path FROM review_sessions WHERE review_id=?1",
            [opened.review_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    drop(connection);
    let selection = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(selection.grant_id).unwrap().workspace_session_id;
    assert!(drafts.read(session, opened.review_id).is_err());
    let recovered = drafts.read(reopened, opened.review_id).unwrap().unwrap();
    assert_eq!(recovered.receipt, current);
    assert_eq!(recovered.coco_document["annotations"], json!([]));
    let artifact = root.join(current_path);
    let bytes = fs::read(&artifact).unwrap();
    let mut modified = bytes.clone();
    modified.push(b' ');
    fs::write(&artifact, &modified).unwrap();
    assert!(drafts.read(reopened, opened.review_id).is_err());
    fs::write(&artifact, &bytes).unwrap();
    owner
        .cancel_review(reopened, opened.review_id, current.review_revision, 20)
        .unwrap();
    assert!(drafts.read(reopened, opened.review_id).unwrap().is_none());
    assert_eq!(fs::read(&formal).unwrap(), ORIGINAL);
    owner.close(reopened).unwrap();
}
