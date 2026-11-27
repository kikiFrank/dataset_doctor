use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::workflow_store::{
    LocalHealthcheckRequest, LocalHealthcheckRuntime, WorkflowRecheckRequest,
    WorkflowReviewCheckAcknowledgment, WorkflowStatus,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const ANNOTATION: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

async fn reviewed_case(
    case: &str,
) -> (
    WorkflowFixture,
    LocalHealthcheckRuntime,
    WorkflowRecheckRequest,
    Uuid,
    Uuid,
) {
    let fixture = WorkflowFixture::with_samples(case, ANNOTATION, json!({}), &["signal.png"]);
    let owner = &fixture.workspace;
    let session = fixture.session;
    let runtime = LocalHealthcheckRuntime::new(
        Arc::clone(owner),
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .unwrap();
    let admitted = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 1,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let run = admitted.workflow.run_id;
    let initial = runtime.run(admitted.job.unwrap()).await.unwrap();
    let waiting = fixture.store.get(session, run).unwrap();
    let asset = waiting.geometry_tasks[0].asset_id;
    let original_geometry = waiting.geometry_tasks[0].task_id;
    let request = WorkflowRecheckRequest {
        workflow_run_id: run,
        expected_row_revision: waiting.row_revision,
        expected_report: initial.reference.clone(),
        acknowledged_incomplete_checks: initial
            .data
            .unexecuted_checks
            .iter()
            .map(|check| WorkflowReviewCheckAcknowledgment {
                check_id: check.clone(),
                reason: "Confirmed the declared local coverage".into(),
            })
            .collect(),
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let count = || {
        db.query_row(
            "SELECT count(*) FROM tasks WHERE capability_id='annotation_geometry.v1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert!(
        runtime
            .complete_after_review(session, request.clone())
            .await
            .is_err()
    );
    assert_eq!(count(), 1);

    let review = owner
        .open_review(session, fixture.dataset, asset, now())
        .unwrap();
    let categories = owner
        .dataset_detail(session, fixture.dataset)
        .unwrap()
        .categories;
    assert_eq!(categories.len(), 1);
    let category = &categories[0];
    let mut corrected: Value = serde_json::from_slice(ANNOTATION).unwrap();
    corrected["categories"] = json!([{"id":category.id,"name":category.name}]);
    corrected["annotations"][0]["category_id"] = json!(category.id);
    corrected["annotations"][0]["bbox"] = json!([0, 0, 2, 1]);
    corrected["annotations"][0]["area"] = json!(2);
    let draft = ReviewDraftService::new(Arc::clone(owner))
        .unwrap()
        .save(
            session,
            ReviewDraftRequest {
                review_id: review.review_id,
                source_task_id: original_geometry,
                expected_review_revision: review.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&corrected).unwrap(),
            },
            now(),
        )
        .unwrap();
    let finals = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let final_operation = finals
        .prepare(
            session,
            ReviewFinalRequest {
                review_id: review.review_id,
                expected_review_revision: draft.review_revision,
                base_image_digest: review.base_image_digest,
                base_annotation_digest: review.base_annotation_digest,
                draft_digest: draft.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: false,
            },
            now(),
        )
        .unwrap();
    assert_eq!(
        finals
            .resume(session, final_operation.operation_id, now())
            .unwrap()
            .state,
        FinalOperationState::Committed
    );

    let mut unacknowledged = request.clone();
    unacknowledged.acknowledged_incomplete_checks.clear();
    assert!(
        runtime
            .complete_after_review(session, unacknowledged)
            .await
            .is_err()
    );
    assert_eq!(count(), 1);
    (
        fixture,
        runtime,
        request,
        final_operation.operation_id,
        original_geometry,
    )
}

#[tokio::test]
async fn review_completion_runs_one_real_recheck_and_replays_without_new_tasks() {
    let (fixture, runtime, request, final_operation_id, original_geometry) =
        reviewed_case("Workflow Recheck Dispatch").await;
    let owner = &fixture.workspace;
    let session = fixture.session;
    let run = request.workflow_run_id;
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let count = || {
        db.query_row(
            "SELECT count(*) FROM tasks WHERE capability_id='annotation_geometry.v1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let finished = runtime
        .complete_after_review(session, request.clone())
        .await
        .unwrap();
    assert_eq!(
        fixture.store.get(session, run).unwrap().status,
        WorkflowStatus::Completed
    );
    assert_eq!(finished.data.repair_outcomes.len(), 1);
    let associated = &finished.data.repair_outcomes[0];
    assert_eq!(
        associated["final_operation_id"],
        final_operation_id.to_string()
    );
    let recheck = associated["geometry_task_id"].as_str().unwrap();
    assert_ne!(recheck, original_geometry.to_string());
    assert_eq!(count(), 2);
    assert_eq!(
        db.query_row(
            "SELECT status,attempt_no FROM tasks WHERE task_id=?1",
            [recheck],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
        )
        .unwrap(),
        ("SUCCEEDED".into(), 1)
    );
    let persisted = fs::read(fixture.source.join("signal.json")).unwrap();

    owner.close(session).unwrap();
    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        runtime
            .complete_after_review(
                reopened,
                WorkflowRecheckRequest {
                    request_id: Uuid::new_v4(),
                    ..request.clone()
                }
            )
            .await
            .unwrap()
            .reference,
        finished.reference
    );
    let mut different = request;
    different.acknowledged_incomplete_checks[0].reason = "A different acknowledgment".into();
    assert!(
        runtime
            .complete_after_review(reopened, different)
            .await
            .is_err()
    );
    assert_eq!(count(), 2);
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        persisted
    );
    owner.close(reopened).unwrap();
}

#[tokio::test]
async fn queued_rechecks_roll_back_atomically_and_resume_after_reopen() {
    let (fixture, runtime, request, _, _) = reviewed_case("Workflow Recheck Recovery").await;
    let owner = &fixture.workspace;
    let session = fixture.session;
    let run = request.workflow_run_id;
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_recheck BEFORE UPDATE OF node_states_json ON workflow_runs BEGIN SELECT RAISE(ABORT,'synthetic recheck failure'); END;").unwrap();
    assert!(
        runtime
            .prepare_review_completion(session, request.clone())
            .is_err()
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM tasks WHERE capability_id='annotation_geometry.v1'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        fixture.store.get(session, run).unwrap().row_revision,
        request.expected_row_revision
    );
    db.execute_batch("DROP TRIGGER fail_recheck").unwrap();
    let queued = runtime
        .prepare_review_completion(session, request.clone())
        .unwrap();
    let task: String = db.query_row("SELECT task_id FROM tasks WHERE workflow_run_id=?1 AND node_id LIKE 'final_annotation/recheck/%'", [run.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(
        owner
            .task_outcome(session, task.parse().unwrap())
            .unwrap()
            .status,
        "QUEUED"
    );
    drop(queued);
    owner.close(session).unwrap();
    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(
        runtime
            .prepare_review_completion(session, request.clone())
            .is_err()
    );
    let mut stale = request.clone();
    stale.expected_row_revision += 1;
    assert!(runtime.prepare_review_completion(reopened, stale).is_err());
    let (first, second) = tokio::join!(
        runtime.complete_after_review(reopened, request.clone()),
        runtime.complete_after_review(reopened, request),
    );
    assert_eq!(first.unwrap().reference, second.unwrap().reference);
    assert_eq!(
        fixture.store.get(reopened, run).unwrap().status,
        WorkflowStatus::Completed
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM tasks WHERE capability_id='annotation_geometry.v1'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT attempt_no FROM tasks WHERE task_id=?1",
            [task],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    owner.close(reopened).unwrap();
}

#[tokio::test]
async fn an_explicit_review_request_can_start_after_reopening_the_final_workspace() {
    let (fixture, runtime, request, _, _) = reviewed_case("Workflow Recheck Reopened Final").await;
    fixture.workspace.close(fixture.session).unwrap();
    let grant = fixture
        .workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = fixture
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert!(
        runtime
            .prepare_review_completion(fixture.session, request.clone())
            .is_err()
    );
    let finished = runtime
        .complete_after_review(reopened, request.clone())
        .await
        .unwrap();
    assert_eq!(finished.data.human_ack_status, "ACKNOWLEDGED");
    assert_eq!(
        fixture
            .store
            .get(reopened, request.workflow_run_id)
            .unwrap()
            .status,
        WorkflowStatus::Completed
    );
    fixture.workspace.close(reopened).unwrap();
}

#[tokio::test]
async fn changed_inputs_and_failed_real_rechecks_cannot_complete_the_workflow() {
    let (fixture, runtime, request, _, _) = reviewed_case("Workflow Recheck Failure").await;
    let session = fixture.session;
    let run = request.workflow_run_id;
    let source = fixture.source.join("signal.png");
    let original = fs::read(&source).unwrap();
    fs::write(&source, b"changed synthetic image").unwrap();
    assert!(
        runtime
            .prepare_review_completion(session, request.clone())
            .is_err()
    );
    fs::write(&source, &original).unwrap();

    let job = runtime
        .prepare_review_completion(session, request.clone())
        .unwrap();
    let wrapper = fixture.root.join("fail-recheck.py");
    let entry =
        serde_json::to_string(fixture.entrypoint.parent().unwrap().to_str().unwrap()).unwrap();
    fs::write(&wrapper, format!("import sys\nsys.path.insert(0, {entry})\nimport entrypoint\ndef fail(request):\n    raise RuntimeError('synthetic failure')\nentrypoint.geometry_execution.compute_result = fail\nraise SystemExit(entrypoint.main())\n")).unwrap();
    let failing = LocalHealthcheckRuntime::new(
        Arc::clone(&fixture.workspace),
        fixture.interpreter.clone(),
        wrapper,
    )
    .unwrap();
    assert!(failing.run_review_completion(job).await.is_err());
    assert_eq!(
        fixture.store.get(session, run).unwrap().status,
        WorkflowStatus::WaitingReview
    );
    assert_eq!(
        fixture.store.read_report(session, run).unwrap().reference,
        request.expected_report
    );
    assert!(
        runtime
            .complete_after_review(session, request.clone())
            .await
            .is_err()
    );
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let (status, error, attempts): (String, String, u32) = db.query_row("SELECT status,error_code,attempt_no FROM tasks WHERE workflow_run_id=?1 AND node_id LIKE 'final_annotation/recheck/%'", [run.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(
        (status.as_str(), error.as_str(), attempts),
        ("FAILED", "INTERNAL_ERROR", 1)
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    fixture.workspace.close(session).unwrap();
    let grant = fixture
        .workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let session = fixture
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    let mut retry = request;
    retry.expected_row_revision = fixture.store.get(session, run).unwrap().row_revision;
    retry.idempotency_key = Uuid::new_v4();
    retry.request_id = Uuid::new_v4();
    let recovered = runtime.complete_after_review(session, retry).await.unwrap();
    assert_eq!(recovered.data.human_ack_status, "ACKNOWLEDGED");
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM tasks WHERE capability_id='annotation_geometry.v1'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        3
    );
    fixture.workspace.close(session).unwrap();
}
