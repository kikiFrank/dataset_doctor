use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::report_export::{ReportExportFormat, ReportExportRequest, ReportExportService};
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::workflow_store::{
    LocalHealthcheckRequest, LocalHealthcheckRuntime, WorkflowReviewCheckAcknowledgment,
    WorkflowReviewEvidence, WorkflowReviewRequest, WorkflowStatus,
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

#[tokio::test]
async fn workflow_completes_only_after_explicit_final_current_recheck_and_gap_acknowledgment() {
    let fixture = WorkflowFixture::with_samples(
        "Workflow Final Gate",
        ANNOTATION,
        json!({}),
        &["signal.png"],
    );
    let owner = &fixture.workspace;
    let session = fixture.session;
    let store = &fixture.store;
    let runtime = LocalHealthcheckRuntime::new(
        Arc::clone(owner),
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .unwrap();
    let accepted = runtime
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
    let run = accepted.workflow.run_id;
    let original = runtime.run(accepted.job.unwrap()).await.unwrap();
    let waiting = store.get(session, run).unwrap();
    assert_eq!(waiting.status, WorkflowStatus::WaitingReview);
    assert_eq!(original.data.human_ack_status, "PENDING");
    assert!(
        original
            .data
            .unexecuted_checks
            .iter()
            .any(|check| check == "semantic_review")
    );
    let issue = original
        .data
        .checks
        .iter()
        .flat_map(|check| &check.findings)
        .find(|finding| finding.finding_code == "bbox_out_of_bounds")
        .unwrap();
    let asset = issue.asset_id;
    let old_geometry = waiting
        .geometry_tasks
        .iter()
        .find(|task| task.asset_id == asset)
        .unwrap()
        .task_id;
    let original_path = fixture
        .root
        .join(format!("artifacts/tasks/{}/report.json", original.task_id));
    let original_bytes = fs::read(&original_path).unwrap();
    let mut request = WorkflowReviewRequest {
        workflow_run_id: run,
        expected_row_revision: waiting.row_revision,
        expected_report: original.reference.clone(),
        evidence: vec![WorkflowReviewEvidence {
            asset_id: asset,
            final_operation_id: Uuid::new_v4(),
            geometry_task_id: old_geometry,
        }],
        acknowledged_incomplete_checks: original
            .data
            .unexecuted_checks
            .iter()
            .map(|check| WorkflowReviewCheckAcknowledgment {
                check_id: check.clone(),
                reason: "Reviewed the declared coverage gap for this synthetic sample".into(),
            })
            .collect(),
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    assert!(
        store
            .complete_review(session, request.clone(), now())
            .is_err()
    );

    let open = owner
        .open_review(session, fixture.dataset, asset, now())
        .unwrap();
    let mut corrected: Value = serde_json::from_slice(ANNOTATION).unwrap();
    let dataset = owner.dataset_detail(session, fixture.dataset).unwrap();
    assert_eq!(dataset.categories.len(), 1);
    let category = &dataset.categories[0];
    corrected["categories"] = json!([{"id": category.id, "name": category.name}]);
    corrected["annotations"][0]["category_id"] = json!(category.id);
    corrected["annotations"][0]["bbox"] = json!([0, 0, 2, 1]);
    corrected["annotations"][0]["area"] = json!(2);
    let drafts = ReviewDraftService::new(Arc::clone(owner)).unwrap();
    let saved = drafts
        .save(
            session,
            ReviewDraftRequest {
                review_id: open.review_id,
                source_task_id: old_geometry,
                expected_review_revision: open.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&corrected).unwrap(),
            },
            now(),
        )
        .unwrap();
    let finals = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let prepared = finals
        .prepare(
            session,
            ReviewFinalRequest {
                review_id: open.review_id,
                expected_review_revision: saved.review_revision,
                base_image_digest: open.base_image_digest,
                base_annotation_digest: open.base_annotation_digest,
                draft_digest: saved.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: false,
            },
            now(),
        )
        .unwrap();
    assert_eq!(
        finals
            .resume(session, prepared.operation_id, now())
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    request.evidence[0].final_operation_id = prepared.operation_id;
    assert!(
        store
            .complete_review(session, request.clone(), now())
            .is_err()
    );
    assert_eq!(
        store.get(session, run).unwrap().status,
        WorkflowStatus::WaitingReview
    );

    let mut geometry = GeometryRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter,
        fixture.entrypoint,
    )
    .await
    .unwrap();
    let recheck = geometry
        .audit(GeometryAuditRequest {
            dataset_id: fixture.dataset,
            asset_id: asset,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            max_pixels: 100,
        })
        .await
        .unwrap();
    assert_eq!(recheck.status, "SUCCEEDED");
    geometry.shutdown().await.unwrap();
    request.evidence[0].geometry_task_id = recheck.task_id;
    let mut missing_ack = request.clone();
    missing_ack.acknowledged_incomplete_checks.clear();
    assert!(store.complete_review(session, missing_ack, now()).is_err());

    let formal = fixture.source.join("signal.json");
    let final_bytes = fs::read(&formal).unwrap();
    assert_ne!(final_bytes, ANNOTATION);
    fs::write(&formal, b"changed after the recheck").unwrap();
    assert!(
        store
            .complete_review(session, request.clone(), now())
            .is_err()
    );
    fs::write(&formal, &final_bytes).unwrap();

    {
        let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_workflow_completion BEFORE UPDATE OF status ON workflow_runs
             WHEN NEW.status='COMPLETED'
             BEGIN SELECT RAISE(ABORT, 'synthetic completion write failure'); END;",
        )
        .unwrap();
        assert!(
            store
                .complete_review(session, request.clone(), now())
                .is_err()
        );
        assert_eq!(
            store.get(session, run).unwrap().status,
            WorkflowStatus::WaitingReview
        );
        assert_eq!(
            store.read_report(session, run).unwrap().reference,
            original.reference
        );
        assert_eq!(db.query_row(
            "SELECT count(*) FROM tasks WHERE workflow_run_id=?1 AND capability_id='workflow_review_complete.v1'",
            [run.to_string()], |row| row.get::<_, i64>(0),
        ).unwrap(), 0);
        db.execute_batch("DROP TRIGGER fail_workflow_completion")
            .unwrap();
    }

    let completed = store
        .complete_review(session, request.clone(), now())
        .unwrap();
    assert_eq!(
        store.get(session, run).unwrap().status,
        WorkflowStatus::Completed
    );
    assert_eq!(completed.data.human_ack_status, "ACKNOWLEDGED");
    assert_eq!(completed.data.scope.workflow_run_id, run);
    assert_eq!(
        completed.data.unexecuted_checks,
        original.data.unexecuted_checks
    );
    assert_eq!(completed.data.repair_outcomes.len(), 1);
    assert_ne!(completed.reference, original.reference);
    assert_eq!(
        store
            .complete_review(session, request.clone(), now())
            .unwrap()
            .reference,
        completed.reference
    );
    assert_eq!(fs::read(original_path).unwrap(), original_bytes);
    assert_eq!(fs::read(formal).unwrap(), final_bytes);
    owner.close(session).unwrap();

    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(
        store
            .complete_review(session, request.clone(), now())
            .is_err()
    );
    assert_eq!(
        store
            .complete_review(reopened, request, now())
            .unwrap()
            .reference,
        completed.reference
    );
    assert_eq!(
        store.read_report(reopened, run).unwrap().reference,
        completed.reference
    );
    assert_eq!(
        store.get(reopened, run).unwrap().status,
        WorkflowStatus::Completed
    );
    let export_directory = fixture.root.parent().unwrap().join("Review Gate Reports");
    fs::create_dir(&export_directory).unwrap();
    let exporter = ReportExportService::new(Arc::clone(owner)).unwrap();
    let completed_bytes = fs::read(
        fixture
            .root
            .join(format!("artifacts/tasks/{}/report.json", completed.task_id)),
    )
    .unwrap();
    for (reference, bytes) in [
        (original.reference, original_bytes),
        (completed.reference, completed_bytes),
    ] {
        let grant = owner
            .grant_export_directory(reopened, export_directory.clone())
            .unwrap();
        let (task, job) = exporter
            .submit(
                reopened,
                ReportExportRequest {
                    report_ref: reference.clone(),
                    format: ReportExportFormat::Json,
                    destination_grant_id: grant.grant_id,
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                },
            )
            .unwrap();
        exporter.run(reopened, job.unwrap()).unwrap();
        let accepted = exporter
            .get(reopened, task.task_id)
            .unwrap()
            .receipt
            .unwrap();
        assert_eq!(accepted.report_ref, reference);
        assert_eq!(
            fs::read(export_directory.join(accepted.file_name)).unwrap(),
            bytes
        );
    }
    owner.close(reopened).unwrap();
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}
