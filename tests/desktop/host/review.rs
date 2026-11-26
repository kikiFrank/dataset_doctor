use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::annotation::{
    AnnotationGetRequest, ReviewCancelRequest, ReviewOpenRequest, ReviewSaveDraftRequest,
};
use dd_ui_contracts::dataset_import::{GeometrySubmitRequest, TaskGetRequest};

use super::{geometry::GeometryRunner, *};

#[test]
fn desktop_review_checks_baselines_and_restores_validated_draft() {
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
    let root = run.join("Desktop review Workspace");
    let input = run.join("Desktop review inputs");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&input).unwrap();
    fs::write(
        input.join("image.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    let formal_path = input.join("image.json");
    let formal = br#"{"images":[{"id":4,"file_name":"image.png","width":2,"height":1}],"annotations":[{"id":7,"image_id":4,"category_id":19,"bbox":[0,0,3,1],"area":3,"iscrowd":0}],"categories":[{"id":19,"name":"helmet"}]}"#;
    fs::write(&formal_path, formal).unwrap();

    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = thread.grant_selected_directory(root.clone()).unwrap();
    let workspace = thread.create(grant.grant_id, "Review".into(), 1).unwrap();
    let session = workspace.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap();
    let grant = thread.grant_import_directory(session, input).unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Images".into())
        .unwrap();
    let receipt = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap();
    let dataset_id = Uuid::parse_str(&receipt.dataset_id).unwrap();
    let asset_id = thread
        .list_assets(session, dataset_id, None, 1)
        .unwrap()
        .items[0]
        .asset_id;
    let id = |value: Uuid| UuidText::try_from(value.to_string()).unwrap();
    let context = |session| CommandContext {
        request_id: id(Uuid::new_v4()),
        workspace_id: UuidText::try_from(workspace.workspace_id.clone()).unwrap(),
        workspace_session_id: id(session),
        idempotency_key: None,
    };
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: id(Uuid::new_v4()),
    };
    let formal_view = commands
        .annotation(AnnotationGetRequest {
            context: context(session),
            asset_id: id(asset_id),
        })
        .unwrap();
    let open = |session, image_digest| ReviewOpenRequest {
        context: context(session),
        asset_id: id(asset_id),
        dataset_id: id(dataset_id),
        base_image_digest: image_digest,
        base_annotation_digest: formal_view.annotation_digest.clone(),
    };
    let stale = commands
        .open_review(open(session, "0".repeat(64)))
        .unwrap_err();
    assert_eq!(stale.code, "CONFLICT");
    assert!(
        commands
            .annotation(AnnotationGetRequest {
                context: context(session),
                asset_id: id(asset_id),
            })
            .unwrap()
            .active_review
            .is_none()
    );

    let mut submit_context = context(session);
    submit_context.idempotency_key = Some(id(Uuid::new_v4()));
    let operation = commands
        .start_geometry(
            GeometryRunner::new(
                repo.join("target/dev/worker-venv/Scripts/python.exe"),
                repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
            ),
            GeometrySubmitRequest {
                context: submit_context,
                dataset_id: id(dataset_id),
                asset_id: id(asset_id),
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let outcome = commands
            .import_task(
                &imports,
                TaskGetRequest {
                    context: context(session),
                    task_id: operation.task_id.clone(),
                },
            )
            .unwrap();
        if !matches!(
            outcome.status.as_str(),
            "QUEUED" | "DISPATCHING" | "RUNNING"
        ) {
            assert_eq!(outcome.status, "SUCCEEDED");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }

    let editor = commands
        .open_review(open(session, formal_view.image_digest.clone()))
        .unwrap();
    assert!(editor.draft.is_none());
    assert_eq!(editor.categories[0].name, "helmet");
    let registered_category = editor.categories[0].id;
    assert_ne!(registered_category, 19);
    let document = |width, category_id| {
        serde_json::from_value(serde_json::json!({
        "images":[{"id":4,"file_name":"image.png","width":2,"height":1}],
        "annotations":[{"id":7,"image_id":4,"category_id":category_id,"bbox":[0,0,width,1],"area":3,"iscrowd":0}],
        "categories":[{"id":category_id,"name":"helmet"}]
    })).unwrap()
    };
    let save = |coco_document| ReviewSaveDraftRequest {
        context: context(session),
        review_id: editor.review.review_id.clone(),
        source_task_id: operation.task_id.clone(),
        expected_review_revision: editor.review.row_revision.clone(),
        expected_draft_revision: None,
        coco_document,
    };
    assert_eq!(
        commands
            .save_review_draft(save(document(3, registered_category)))
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        commands
            .save_review_draft(save(document(2, 19)))
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
    let receipt = commands
        .save_review_draft(save(document(2, registered_category)))
        .unwrap();
    assert_eq!(
        commands
            .save_review_draft(save(document(1, registered_category)))
            .unwrap_err()
            .code,
        "CONFLICT"
    );
    assert_eq!(fs::read(&formal_path).unwrap(), formal);

    thread.close(session).unwrap();
    let grant = thread.grant_selected_directory(root).unwrap();
    let reopened = thread.open(grant.grant_id).unwrap();
    let current_session = reopened.workspace_session_id;
    let restored = commands
        .open_review(open(current_session, formal_view.image_digest.clone()))
        .unwrap();
    assert_eq!(restored.review.review_id, editor.review.review_id);
    let draft = restored.draft.unwrap();
    assert_eq!(draft.receipt.draft_digest, receipt.draft_digest);
    assert_eq!(draft.source_task_id, operation.task_id);
    assert_eq!(draft.coco_document.annotations[0].area, 2.0);
    let cancel = |session| ReviewCancelRequest {
        context: context(session),
        review_id: restored.review.review_id.clone(),
        expected_revision: restored.review.row_revision.clone(),
    };
    assert_eq!(
        commands.cancel_review(cancel(session)).unwrap_err().code,
        "STALE_WORKSPACE_SESSION"
    );
    let cancelled = commands.cancel_review(cancel(current_session)).unwrap();
    assert_eq!(cancelled.status, "CANCELLED");
    assert_eq!(fs::read(&formal_path).unwrap(), formal);
    thread.close(current_session).unwrap();
}
