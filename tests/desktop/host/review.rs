use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::annotation::{
    AnnotationGetRequest, CocoDocument, ReviewCancelRequest, ReviewCommitRequest,
    ReviewOpenRequest, ReviewSaveDraftRequest,
};
use dd_ui_contracts::dataset::AnnotationState;
use dd_ui_contracts::dataset_export::{
    DatasetExportFormat, DatasetExportSelection, DatasetExportSubmitRequest, ExportPolicy,
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
    fs::copy(input.join("image.png"), input.join("unreviewed.png")).unwrap();
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
        .list_assets(session, dataset_id, None, 2)
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.filename == "image.png")
        .unwrap()
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
        expected_review_revision: editor.review.row_revision,
        expected_draft_revision: None,
        coco_document,
        track_selections: None,
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
    let grant = thread.grant_selected_directory(root.clone()).unwrap();
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
        expected_revision: restored.review.row_revision,
    };
    assert_eq!(
        commands.cancel_review(cancel(session)).unwrap_err().code,
        "STALE_WORKSPACE_SESSION"
    );
    let cancelled = commands.cancel_review(cancel(current_session)).unwrap();
    assert_eq!(cancelled.status, "CANCELLED");
    assert_eq!(fs::read(&formal_path).unwrap(), formal);

    let editor = commands
        .open_review(open(current_session, formal_view.image_digest.clone()))
        .unwrap();
    let mut empty: CocoDocument = document(2, registered_category);
    empty.annotations.clear();
    let draft = commands
        .save_review_draft(ReviewSaveDraftRequest {
            context: context(current_session),
            review_id: editor.review.review_id.clone(),
            source_task_id: operation.task_id,
            expected_review_revision: editor.review.row_revision,
            expected_draft_revision: None,
            coco_document: empty,
            track_selections: None,
        })
        .unwrap();
    let key = id(Uuid::new_v4());
    assert!(editor.annotation_write_access.authorization_required);
    let write_grant = commands
        .authorize_review_write(
            context(current_session),
            editor.review.review_id.clone(),
            formal_path.parent().unwrap().to_owned(),
        )
        .unwrap();
    assert!(
        write_grant
            .allowed_operations
            .contains(&"REVIEW_COMMIT".into())
    );
    let commit = |confirmed_negative| {
        let mut context = context(current_session);
        context.idempotency_key = Some(key.clone());
        ReviewCommitRequest {
            annotation_write_grant_id: Some(write_grant.grant_id.clone()),
            context,
            review_id: editor.review.review_id.clone(),
            expected_review_revision: draft.review_revision,
            base_image_digest: editor.review.base_image_digest.clone(),
            base_annotation_digest: editor.review.base_annotation_digest.clone(),
            draft_digest: draft.draft_digest.clone(),
            confirmed_negative,
        }
    };
    assert_eq!(
        commands.commit_review(commit(false)).unwrap_err().code,
        "FAILED_PRECONDITION"
    );
    assert_eq!(fs::read(&formal_path).unwrap(), formal);
    let mut ungranted = commit(true);
    ungranted.annotation_write_grant_id = None;
    assert_eq!(
        commands.commit_review(ungranted).unwrap_err().code,
        "PERMISSION_DENIED"
    );
    let accepted = commands.commit_review(commit(true)).unwrap();
    assert_eq!(accepted.status, "QUEUED");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let outcome = commands
            .import_task(
                &imports,
                TaskGetRequest {
                    context: context(current_session),
                    task_id: accepted.task_id.clone(),
                },
            )
            .unwrap();
        if !matches!(outcome.status.as_str(), "QUEUED" | "RUNNING") {
            assert_eq!(outcome.status, "SUCCEEDED");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    let confirmed = commands
        .annotation(AnnotationGetRequest {
            context: context(current_session),
            asset_id: id(asset_id),
        })
        .unwrap();
    assert_eq!(
        confirmed.annotation_state,
        AnnotationState::ConfirmedNegative
    );
    assert!(confirmed.active_review.is_none());
    assert!(confirmed.formal_coco.unwrap().annotations.is_empty());
    assert_ne!(fs::read(&formal_path).unwrap(), formal);
    let replay = commands.commit_review(commit(true)).unwrap();
    assert_eq!(replay.task_id, accepted.task_id);
    assert_eq!(replay.status, "SUCCEEDED");
    assert_eq!(
        commands.commit_review(commit(false)).unwrap_err().code,
        "CONFLICT"
    );

    let destination = run.join("Desktop COCO export");
    fs::create_dir(&destination).unwrap();
    let build = |grant_id, policy, key| {
        let mut context = context(current_session);
        context.idempotency_key = Some(id(key));
        DatasetExportSubmitRequest {
            context,
            dataset_id: id(dataset_id),
            destination_grant_id: grant_id,
            format: DatasetExportFormat::CocoDetection,
            selection: DatasetExportSelection::AllMembers,
            export_policy: policy,
        }
    };
    let read_grant = thread
        .grant_import_directory(current_session, destination.clone())
        .unwrap();
    let forbidden = commands
        .submit_export(build(
            id(read_grant.grant_id),
            ExportPolicy::Strict,
            Uuid::new_v4(),
        ))
        .unwrap_err();
    assert_eq!(forbidden.code, "PERMISSION_DENIED");
    let grant = commands
        .export_directory(context(current_session), destination.clone())
        .unwrap();
    let wait = |task_id: UuidText| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let result = commands
                .import_task(
                    &imports,
                    TaskGetRequest {
                        context: context(current_session),
                        task_id: task_id.clone(),
                    },
                )
                .unwrap();
            if !matches!(result.status.as_str(), "QUEUED" | "RUNNING") {
                break result;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let strict = commands
        .submit_export(build(grant.grant_id, ExportPolicy::Strict, Uuid::new_v4()))
        .unwrap();
    let result = wait(strict.task_id);
    assert_eq!(result.status, "FAILED");
    assert_eq!(result.error_code.as_deref(), Some("EXPORT_BLOCKED"));
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);

    let grant = commands
        .export_directory(context(current_session), destination.clone())
        .unwrap();
    let key = Uuid::new_v4();
    let accepted = commands
        .submit_export(build(
            grant.grant_id.clone(),
            ExportPolicy::ExcludeIneligible,
            key,
        ))
        .unwrap();
    assert_eq!(accepted.status, "QUEUED");
    let result = wait(accepted.task_id.clone());
    assert_eq!(result.status, "SUCCEEDED");
    let receipt = result.coco_export.unwrap();
    assert_eq!(receipt.image_count, 1_u64.into());
    assert_eq!(receipt.annotation_count, 0_u64.into());
    assert_eq!(receipt.excluded_count, 1_u64.into());
    let package = PathBuf::from(receipt.package_path).canonicalize().unwrap();
    assert_eq!(
        package.parent(),
        Some(destination.canonicalize().unwrap().as_path())
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(package.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["excluded"][0]["reason"], "UNCONFIRMED");
    let replay = commands
        .submit_export(build(
            grant.grant_id.clone(),
            ExportPolicy::ExcludeIneligible,
            key,
        ))
        .unwrap();
    assert_eq!(replay.task_id, accepted.task_id);
    assert_eq!(replay.status, "SUCCEEDED");
    assert_eq!(
        commands
            .submit_export(build(grant.grant_id, ExportPolicy::Strict, key))
            .unwrap_err()
            .code,
        "CONFLICT"
    );
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    fs::write(
        run.join("export-reader-package.txt"),
        package.to_str().unwrap(),
    )
    .unwrap();
    thread.close(current_session).unwrap();

    let grant = thread.grant_selected_directory(root).unwrap();
    let reopened = thread.open(grant.grant_id).unwrap().workspace_session_id;
    let recovered = commands
        .import_task(
            &imports,
            TaskGetRequest {
                context: context(reopened),
                task_id: accepted.task_id,
            },
        )
        .unwrap();
    assert_eq!(recovered.status, "SUCCEEDED");
    assert_eq!(
        PathBuf::from(recovered.coco_export.unwrap().package_path)
            .canonicalize()
            .unwrap(),
        package
    );
    let image_path = formal_path.with_extension("png");
    let before = commands
        .annotation(AnnotationGetRequest {
            context: context(reopened),
            asset_id: id(asset_id),
        })
        .unwrap();
    let before_bytes = fs::read(&image_path).unwrap();
    let formal_bytes = fs::read(&formal_path).unwrap();
    let color_context = || {
        let mut value = context(reopened);
        value.idempotency_key = Some(id(Uuid::new_v4()));
        value
    };
    let preview_request = |image_digest: &str| {
        serde_json::from_value::<dd_ui_contracts::color::ColorPreviewSubmitRequest>(serde_json::json!({
        "context":color_context(),"asset_id":asset_id,"dataset_id":dataset_id,
        "base_image_digest":image_digest,"region":{"kind":"RECT","bbox_xywh":[0,0,1,1]},
        "operation":{"method":"SOLID_FILL","target_rgb":[17,30,90],"strength":1,"feather_px":0}
    })).unwrap()
    };
    let runner = super::color::ColorRunner::new(
        repo.join("target/dev/worker-venv/Scripts/python.exe"),
        repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
    );
    let submitted = commands
        .start_color_preview(Some(runner), preview_request(&before.image_digest))
        .unwrap();
    let wait_color = |task_id: UuidText| {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let reply = commands
                .import_task(
                    &imports,
                    TaskGetRequest {
                        context: context(reopened),
                        task_id: task_id.clone(),
                    },
                )
                .unwrap();
            if !matches!(reply.status.as_str(), "QUEUED" | "DISPATCHING" | "RUNNING") {
                break reply;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let done = wait_color(submitted.task_id.clone());
    assert_eq!(done.status, "SUCCEEDED");
    let preview = done.color_preview.unwrap();
    assert_eq!(preview.changed_pixels, 1u64.into());
    assert_eq!(fs::read(&image_path).unwrap(), before_bytes);
    let display =
        dd_infra::media_display::MediaDisplayService::new(Arc::clone(&thread), GRANT_LIFETIME);
    let read_artifact = |artifact_id: UuidText| {
        let request = dd_ui_contracts::annotation::MediaGetLocatorRequest {
            context: context(reopened),
            asset_id: None,
            artifact_id: Some(artifact_id),
            owner_task_id: Some(submitted.task_id.clone()),
            purpose: dd_ui_contracts::annotation::MediaPurpose::Display,
        };
        let location = commands.media_locator(&display, request).unwrap();
        let response = super::annotation::media_response(
            &display,
            tauri::http::Request::builder()
                .uri(location.locator)
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(response.status(), 200);
        response.into_body()
    };
    let rendered = read_artifact(preview.candidate_ref.artifact_id.clone());
    let mask = read_artifact(preview.mask_ref.artifact_id.clone());
    assert_ne!(rendered, before_bytes);
    assert_ne!(mask, rendered);
    let approval = |reference: &dd_ui_contracts::ArtifactReference| {
        serde_json::from_value::<dd_ui_contracts::color::ColorApplySubmitRequest>(serde_json::json!({
        "context":color_context(),"asset_id":asset_id,"dataset_id":dataset_id,
        "base_image_digest":before.image_digest,"expected_asset_revision":preview.asset_revision,
        "candidate_ref":reference
    })).unwrap()
    };
    assert!(
        commands
            .start_color_apply(approval(&preview.mask_ref))
            .is_err()
    );
    let apply = commands
        .start_color_apply(approval(&preview.candidate_ref))
        .unwrap();
    let applied = wait_color(apply.task_id);
    assert_eq!(applied.status, "SUCCEEDED");
    assert_eq!(
        applied.color_apply.unwrap().image_digest,
        preview.candidate_ref.sha256
    );
    assert_eq!(fs::read(&image_path).unwrap(), rendered);
    assert_eq!(fs::read(&formal_path).unwrap(), formal_bytes);
    let current = commands
        .annotation(AnnotationGetRequest {
            context: context(reopened),
            asset_id: id(asset_id),
        })
        .unwrap();
    assert!(matches!(current.annotation_state, AnnotationState::Stale));
    let unavailable = commands
        .start_color_preview(None, preview_request(&current.image_digest))
        .unwrap();
    assert_eq!(
        wait_color(unavailable.task_id).error_code.as_deref(),
        Some("WORKER_START_FAILED")
    );
    thread.close(reopened).unwrap();
}
