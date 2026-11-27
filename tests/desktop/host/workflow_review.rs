use std::{
    fs,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{ReviewFinalRequest, ReviewFinalService};
use dd_infra::workflow_store::{LocalHealthcheckRequest, LocalHealthcheckRuntime};
use dd_ui_contracts::workflow::{
    WorkflowCompleteReviewRequest, WorkflowGetRequest, WorkflowReportGetRequest, WorkflowStatusView,
};
use serde_json::json;

use super::*;

#[test]
fn desktop_completion_dispatches_the_reviewed_final_and_exposes_its_durable_tasks() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        run.parent(),
        Some(repo.join("target/desktop-commands/runs").as_path())
    );
    let root = run.join("Desktop Workflow Final");
    let source = run.join("Desktop Workflow Final Sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    let png = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
    fs::write(source.join("signal.png"), png).unwrap();
    let annotation = json!({"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],
        "categories":[{"id":7,"name":"signal"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]});
    fs::write(
        source.join("signal.json"),
        serde_json::to_vec(&annotation).unwrap(),
    )
    .unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let opened = owner
        .create(grant.grant_id, "Final review".into(), 1)
        .unwrap();
    let session = opened.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Signals".into())
        .unwrap();
    let dataset = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap()
        .dataset_id
        .parse()
        .unwrap();
    let runtime = Arc::new(
        LocalHealthcheckRuntime::new(
            Arc::clone(&owner),
            repo.join("target/dev/worker-venv/Scripts/python.exe"),
            repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
        )
        .unwrap(),
    );
    let initial = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: dataset,
                sample_limit: 1,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let workflow = initial.workflow.run_id;
    let report = tauri::async_runtime::block_on(runtime.run(initial.job.unwrap())).unwrap();
    let store = dd_infra::workflow_store::WorkflowStore::new(Arc::clone(&owner)).unwrap();
    let waiting = store.get(session, workflow).unwrap();
    let commands = Commands {
        thread: Arc::clone(&owner),
        request_id: UuidText::try_from(Uuid::new_v4().to_string()).unwrap(),
    };
    let key = Uuid::new_v4();
    let context = |session: Uuid| {
        json!({"request_id":Uuid::new_v4(),"workspace_id":opened.workspace_id,
        "workspace_session_id":session,"idempotency_key":key})
    };
    let payload = |session| {
        json!({"context":context(session),"workflow_run_id":workflow,
        "expected_revision":waiting.row_revision.to_string(),"expected_report":report.reference,
        "acknowledged_incomplete_checks":report.data.unexecuted_checks.iter().map(|check| json!({"check_id":check,"reason":"Explicitly accepted local coverage"})).collect::<Vec<_>>()})
    };
    let request = |session| {
        serde_json::from_value::<WorkflowCompleteReviewRequest>(payload(session)).unwrap()
    };
    assert!(
        commands
            .complete_workflow_review(Arc::clone(&runtime), request(session))
            .is_err()
    );

    let asset = waiting.geometry_tasks[0].asset_id;
    let now = || {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    };
    let review = owner.open_review(session, dataset, asset, now()).unwrap();
    let category = owner
        .dataset_detail(session, dataset)
        .unwrap()
        .categories
        .remove(0);
    let mut fixed = annotation;
    fixed["categories"] = json!([{"id":category.id,"name":category.name}]);
    fixed["annotations"][0]["category_id"] = json!(category.id);
    fixed["annotations"][0]["bbox"] = json!([0, 0, 2, 1]);
    fixed["annotations"][0]["area"] = json!(2);
    let draft = ReviewDraftService::new(Arc::clone(&owner))
        .unwrap()
        .save(
            session,
            ReviewDraftRequest {
                review_id: review.review_id,
                source_task_id: waiting.geometry_tasks[0].task_id,
                expected_review_revision: review.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&fixed).unwrap(),
            },
            now(),
        )
        .unwrap();
    let finals = ReviewFinalService::new(Arc::clone(&owner)).unwrap();
    let grant = finals
        .authorize_write(session, review.review_id, source.clone())
        .unwrap();
    let final_operation = finals
        .prepare_with_write_grant(
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
            grant.grant_id,
            now(),
        )
        .unwrap();
    finals
        .resume(session, final_operation.operation_id, now())
        .unwrap();
    let mut missing = payload(session);
    missing["acknowledged_incomplete_checks"] = json!([]);
    assert!(
        commands
            .complete_workflow_review(
                Arc::clone(&runtime),
                serde_json::from_value(missing).unwrap()
            )
            .is_err()
    );
    let accepted = commands
        .complete_workflow_review(Arc::clone(&runtime), request(session))
        .unwrap();
    assert_eq!(accepted.review_recheck_tasks.len(), 1);
    let task = accepted.review_recheck_tasks[0].task_id.clone();
    assert_eq!(
        commands
            .complete_workflow_review(Arc::clone(&runtime), request(session))
            .unwrap()
            .review_recheck_tasks[0]
            .task_id,
        task
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let current = commands
            .get_workflow(WorkflowGetRequest {
                context: serde_json::from_value(context(session)).unwrap(),
                workflow_run_id: UuidText::try_from(workflow.to_string()).unwrap(),
            })
            .unwrap();
        if current.status == WorkflowStatusView::Completed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "review did not finish: {current:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let finished = commands
        .get_workflow_report(WorkflowReportGetRequest {
            context: serde_json::from_value(context(session)).unwrap(),
            workflow_run_id: UuidText::try_from(workflow.to_string()).unwrap(),
        })
        .unwrap();
    assert_eq!(finished.human_ack_status, "ACKNOWLEDGED");
    assert_eq!(
        finished.repair_outcomes[0]["final_operation_id"],
        final_operation.operation_id.to_string()
    );
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(
        commands
            .complete_workflow_review(Arc::clone(&runtime), request(session))
            .is_err()
    );
    let replayed = commands
        .complete_workflow_review(runtime, request(reopened))
        .unwrap();
    assert_eq!(replayed.status, WorkflowStatusView::Completed);
    assert_eq!(replayed.review_recheck_tasks[0].task_id, task);
    assert_eq!(fs::read(source.join("signal.png")).unwrap(), png);
    owner.close(reopened).unwrap();
}
