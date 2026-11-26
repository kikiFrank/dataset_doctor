#[path = "support/review.rs"]
mod review;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{
    FinalOperation, FinalOperationState, ReviewFinalRequest, ReviewFinalService,
};
use dd_infra::review_store::ReviewStatus;
use dd_infra::workspace_thread::WorkspaceThread;
use review::{ORIGINAL, ReviewFixture};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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

    let selection = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(selection.grant_id).unwrap().workspace_session_id;
    request.request_id = Uuid::new_v4();
    assert_eq!(
        final_reviews
            .prepare(reopened, request.clone(), 17)
            .unwrap(),
        accepted
    );
    request.confirmed_negative = false;
    assert!(
        final_reviews
            .prepare(reopened, request.clone(), 18)
            .is_err()
    );
    assert_eq!(fs::read(&formal).unwrap(), ORIGINAL);
    let committed = final_reviews
        .resume(reopened, accepted.operation_id, 19)
        .unwrap();
    assert_eq!(committed.state, FinalOperationState::Committed);
    assert_eq!(
        final_reviews
            .resume(reopened, accepted.operation_id, 20)
            .unwrap(),
        committed
    );
    request.confirmed_negative = true;
    assert_eq!(
        final_reviews.prepare(reopened, request, 21).unwrap(),
        committed
    );
    assert_eq!(
        owner.get_review(reopened, opened.review_id).unwrap().status,
        ReviewStatus::Confirmed
    );
    assert!(drafts.read(reopened, opened.review_id).unwrap().is_none());
    owner.close(reopened).unwrap();
    assert_committed(&root, &formal, opened.review_id);
}

#[tokio::test]
async fn changed_formal_json_after_admission_becomes_conflict_and_stays_untouched() {
    let fixture = ReviewFixture::open().await;
    let (coordinator, operation) = prepare_negative(&fixture);
    let mut outside_edit = ORIGINAL.to_vec();
    outside_edit.push(b' ');
    fs::write(&fixture.formal, &outside_edit).unwrap();
    let result = coordinator
        .resume(fixture.session, operation.operation_id, 20)
        .unwrap();
    assert_eq!(result.state, FinalOperationState::Conflict);
    assert_eq!(
        coordinator
            .resume(fixture.session, operation.operation_id, 21)
            .unwrap(),
        result
    );
    assert_eq!(fs::read(&fixture.formal).unwrap(), outside_edit);
    assert_eq!(
        fixture
            .owner
            .get_review(fixture.session, fixture.opened.review_id)
            .unwrap()
            .status,
        ReviewStatus::Conflict
    );
    let drafts = ReviewDraftService::new(Arc::clone(&fixture.owner)).unwrap();
    assert!(
        drafts
            .read(fixture.session, fixture.opened.review_id)
            .unwrap()
            .is_some()
    );
    fixture.owner.close(fixture.session).unwrap();
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(final_review_id) FROM dataset_assets",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn resumes_a_published_file_after_sqlite_failed_and_the_owner_was_killed() {
    if let Some(root) = std::env::var_os("DD_REVIEW_CRASH_ROOT") {
        let root = PathBuf::from(root);
        let operation =
            Uuid::parse_str(&std::env::var("DD_REVIEW_CRASH_OPERATION").unwrap()).unwrap();
        let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
        let selection = owner.grant_selected_directory(root.clone()).unwrap();
        let opened = owner.open(selection.grant_id).unwrap().workspace_session_id;
        let coordinator = ReviewFinalService::new(owner).unwrap();
        let error = coordinator
            .resume(opened, operation, 21)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("synthetic final transaction failure"),
            "{error}"
        );
        fs::write(
            root.join("sqlite-failure.ready"),
            b"published before transaction",
        )
        .unwrap();
        loop {
            thread::sleep(Duration::from_millis(100));
        }
    }
    let fixture = ReviewFixture::open().await;
    let (coordinator, operation) = prepare_negative(&fixture);
    let queued = coordinator
        .task_for_operation(fixture.session, operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(queued.status, "QUEUED");
    fixture.owner.close(fixture.session).unwrap();
    let db_path = fixture.root.join("workspace.db");
    let db = Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_final BEFORE UPDATE OF final_review_id ON dataset_assets
         BEGIN SELECT RAISE(ABORT, 'synthetic final transaction failure'); END;",
    )
    .unwrap();
    drop(db);
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .arg("--exact")
        .arg("resumes_a_published_file_after_sqlite_failed_and_the_owner_was_killed")
        .env("DD_REVIEW_CRASH_ROOT", &fixture.root)
        .env(
            "DD_REVIEW_CRASH_OPERATION",
            operation.operation_id.to_string(),
        );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        child.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = RecoveryChild(child.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(12);
    while !fixture.root.join("sqlite-failure.ready").exists() {
        assert!(
            Instant::now() < deadline && child.0.try_wait().unwrap().is_none(),
            "Final child did not reach the SQLite rollback"
        );
        thread::sleep(Duration::from_millis(25));
    }
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let published = fs::read(&fixture.formal).unwrap();
    assert_ne!(published, ORIGINAL);
    let db = Connection::open(&db_path).unwrap();
    let (status, final_id, annotation_digest): (String, Option<String>, String) = db
        .query_row(
            "SELECT r.status, m.final_review_id, a.annotation_digest FROM review_sessions r
         JOIN dataset_assets m USING(asset_id) JOIN assets a USING(asset_id) WHERE r.review_id=?1",
            [fixture.opened.review_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "COMMITTING");
    assert!(final_id.is_none());
    assert_eq!(annotation_digest, format!("{:x}", Sha256::digest(ORIGINAL)));
    assert_eq!(
        db.query_row(
            "SELECT status FROM tasks WHERE task_id=?1",
            [queued.task_id.to_string()],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "RUNNING"
    );
    db.execute_batch("DROP TRIGGER fail_final;").unwrap();
    drop(db);

    let selection = fixture
        .owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let session = fixture
        .owner
        .open(selection.grant_id)
        .unwrap()
        .workspace_session_id;
    let recovered = coordinator
        .resume(session, operation.operation_id, 22)
        .unwrap();
    assert_eq!(recovered.state, FinalOperationState::Committed);
    let outcome = coordinator
        .task_snapshot(session, queued.task_id)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.status, "SUCCEEDED");
    assert!(outcome.error_code.is_none());
    assert_eq!(fs::read(&fixture.formal).unwrap(), published);
    fixture.owner.close(session).unwrap();
    assert_committed(&fixture.root, &fixture.formal, fixture.opened.review_id);
}

fn prepare_negative(fixture: &ReviewFixture) -> (ReviewFinalService, FinalOperation) {
    let drafts = ReviewDraftService::new(Arc::clone(&fixture.owner)).unwrap();
    let coordinator = ReviewFinalService::new(Arc::clone(&fixture.owner)).unwrap();
    let mut negative: Value = serde_json::from_slice(ORIGINAL).unwrap();
    negative["annotations"] = json!([]);
    negative["categories"] = json!([]);
    let saved = drafts
        .save(
            fixture.session,
            ReviewDraftRequest {
                review_id: fixture.opened.review_id,
                source_task_id: fixture.task,
                expected_review_revision: 0,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&negative).unwrap(),
            },
            13,
        )
        .unwrap();
    let operation = coordinator
        .prepare(
            fixture.session,
            ReviewFinalRequest {
                review_id: fixture.opened.review_id,
                expected_review_revision: saved.review_revision,
                base_image_digest: fixture.opened.base_image_digest.clone(),
                base_annotation_digest: fixture.opened.base_annotation_digest.clone(),
                draft_digest: saved.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: true,
            },
            14,
        )
        .unwrap();
    (coordinator, operation)
}

fn assert_committed(root: &Path, formal: &Path, review: Uuid) {
    let bytes = fs::read(formal).unwrap();
    let coco: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(coco["annotations"], json!([]));
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let (final_id, status, asset_digest, confirmation_digest, file_status, negative): (String, String, String, String, String, bool) =
        db.query_row(
            "SELECT m.final_review_id, r.status, a.annotation_digest, r.confirmed_annotation_digest, o.status,
                    json_extract(o.result_json,'$.confirmed_negative') FROM dataset_assets m
             JOIN assets a USING(asset_id) JOIN review_sessions r ON r.review_id=m.final_review_id
             JOIN file_operations o ON o.review_id=r.review_id", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).unwrap();
    assert_eq!(
        (final_id, status, file_status, negative),
        (
            review.to_string(),
            "CONFIRMED".into(),
            "COMMITTED".into(),
            true
        )
    );
    assert_eq!(asset_digest, format!("{:x}", Sha256::digest(&bytes)));
    assert_eq!(asset_digest, confirmation_digest);
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

struct RecoveryChild(Child);

impl Drop for RecoveryChild {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
