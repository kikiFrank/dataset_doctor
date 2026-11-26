use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::media_display::MediaDisplayService;
use dd_ui_contracts::annotation::{
    MediaGetLocatorRequest, ReviewCommitRequest, ReviewOpenRequest, ReviewSaveDraftRequest,
};
use dd_ui_contracts::dataset_import::{GeometrySubmitRequest, ImportOperationKind, TaskGetRequest};
use dd_ui_contracts::staging::{
    StagingCommitRequest, StagingIncompleteCheckAck, StagingReportGetRequest, StagingState,
};
use dd_ui_contracts::video::{
    FrameResolution, VideoGetKeyframeDraftRequest, VideoGetRequest, VideoImportRequest,
    VideoResolveFrameRequest, VideoSampleSubmitRequest, VideoSaveKeyframeDraftRequest,
    VideoSaveSegmentRequest,
};
use tauri::http::Request;

use super::{video::VideoRunner, *};

#[path = "../../integration/rpc/support/video_fixture.rs"]
mod video_fixture;

#[test]
fn desktop_resolves_and_displays_a_real_frame_and_reopens_the_verified_cached_result() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let root = run.join("Desktop frame Workspace");
    fs::create_dir(&root).unwrap();
    let clip = run.join("Desktop two color frame.mp4");
    video_fixture::make_clip(&clip);
    let original = fs::read(&clip).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let workspace = owner
        .create(grant.grant_id, "Desktop frames".into(), 100)
        .unwrap();
    let uuid = |value: String| UuidText::try_from(value).unwrap();
    let context = |session: Uuid| CommandContext {
        request_id: uuid(Uuid::new_v4().to_string()),
        workspace_id: uuid(workspace.workspace_id.clone()),
        workspace_session_id: uuid(session.to_string()),
        idempotency_key: Some(uuid(Uuid::new_v4().to_string())),
    };
    let session = workspace.workspace_session_id;
    let commands = Commands {
        thread: Arc::clone(&owner),
        request_id: uuid(Uuid::new_v4().to_string()),
    };
    let runner = VideoRunner::new(
        repo.join("target/dev/worker-venv/Scripts/python.exe"),
        repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
    );
    let grant = commands.video_file(context(session), clip.clone()).unwrap();
    let imported = commands
        .start_video_import(
            Arc::clone(&runner),
            VideoImportRequest {
                context: context(session),
                file_grant_id: grant.grant_id,
                display_name: None,
                stream_index: None,
            },
        )
        .unwrap();
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let wait = |session, mut task_id: UuidText| {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let snapshot = commands
                .import_task(
                    &imports,
                    TaskGetRequest {
                        context: context(session),
                        task_id,
                    },
                )
                .unwrap();
            if !matches!(
                snapshot.status.as_str(),
                "QUEUED" | "DISPATCHING" | "RUNNING"
            ) {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
            task_id = snapshot.task_id;
        }
    };
    assert_eq!(
        wait(session, imported.operation.task_id).status,
        "SUCCEEDED"
    );
    let request = |session: Uuid, target: serde_json::Value| {
        serde_json::from_value::<VideoResolveFrameRequest>(serde_json::json!({
            "context": {
                "request_id": Uuid::new_v4().to_string(),
                "workspace_id": workspace.workspace_id,
                "workspace_session_id": session.to_string(),
                "idempotency_key": Uuid::new_v4().to_string()
            },
            "source_video_id": String::from(imported.source_video_id.clone()),
            "target": target,
            "seek_policy": "AT_OR_AFTER"
        }))
    };
    assert!(
        request(
            session,
            serde_json::json!({"timestamp_ms":"401","frame_index":"0"})
        )
        .is_err()
    );
    let initial_request = request(session, serde_json::json!({"timestamp_ms":"401"})).unwrap();
    let original_key = initial_request.context.idempotency_key.clone();
    let initial = commands
        .resolve_video_frame(Some(Arc::clone(&runner)), initial_request)
        .unwrap();
    let FrameResolution::Accepted { operation } = initial else {
        panic!("first frame request must be queued");
    };
    assert!(matches!(
        operation.operation_kind,
        ImportOperationKind::FrameResolve
    ));
    let done = wait(session, operation.task_id);
    assert_eq!(done.status, "SUCCEEDED");
    let mut conflicting = request(session, serde_json::json!({"frame_index":"0"})).unwrap();
    conflicting.context.idempotency_key = original_key;
    assert_eq!(
        commands
            .resolve_video_frame(Some(Arc::clone(&runner)), conflicting)
            .unwrap_err()
            .code,
        "CONFLICT"
    );
    let frame = done.video_frame.unwrap();
    assert_eq!(String::from(frame.frame_index), "3");
    assert_eq!(String::from(frame.pts_us.unwrap()), "600000");
    let owner_task = match &frame.image_ref.owner {
        dd_ui_contracts::ArtifactOwner::Task(task) => String::from(task.clone()),
        _ => panic!("a resolved frame must belong to its task"),
    };
    let media_request = |session: Uuid, artifact: String| {
        serde_json::from_value::<MediaGetLocatorRequest>(serde_json::json!({
            "context": {
                "request_id": Uuid::new_v4().to_string(),
                "workspace_id": workspace.workspace_id,
                "workspace_session_id": session.to_string()
            },
            "artifact_id": artifact, "owner_task_id": owner_task, "purpose":"DISPLAY"
        }))
        .unwrap()
    };
    let media = MediaDisplayService::new(Arc::clone(&owner), GRANT_LIFETIME);
    assert!(
        commands
            .media_locator(&media, media_request(session, Uuid::new_v4().to_string()))
            .is_err()
    );
    let mut ambiguous = media_request(session, String::from(frame.image_ref.artifact_id.clone()));
    ambiguous.asset_id = Some(imported.source_video_id.clone());
    assert_eq!(
        commands.media_locator(&media, ambiguous).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    let locator = commands
        .media_locator(
            &media,
            media_request(session, String::from(frame.image_ref.artifact_id.clone())),
        )
        .unwrap();
    let response = annotation::media_response(
        &media,
        Request::builder()
            .uri(locator.locator.as_str())
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(response.status(), 200);
    assert!(response.body().starts_with(b"\x89PNG\r\n\x1a\n"));
    let displayed = run.join("Displayed synthetic blue frame.png");
    fs::write(&displayed, response.body()).unwrap();
    let pixels = std::process::Command::new(repo.join("target/dev/worker-venv/Scripts/python.exe"))
        .args(["-I","-B","-c","import sys; from PIL import Image; im=Image.open(sys.argv[1]); im.load(); r,g,b=im.getpixel((20,12)); assert b>240 and max(r,g)<12"])
        .arg(displayed).status().unwrap();
    assert!(pixels.success());
    let input = run.join("Desktop video annotation dataset");
    fs::create_dir(&input).unwrap();
    fs::write(
        input.join("image.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    fs::write(input.join("image.json"), br#"{"images":[{"id":1,"file_name":"image.png","width":2,"height":1}],"annotations":[],"categories":[{"id":2,"name":"signal"}]}"#).unwrap();
    let target = owner.grant_import_directory(session, input).unwrap();
    let preview = imports
        .inspect_directory(session, target.grant_id, "Video labels".into())
        .unwrap();
    let registered = imports
        .confirm(session, preview.plan_id, preview.fingerprint, 200)
        .unwrap();
    let dataset_id = registered.dataset_id.to_string();
    let dataset_detail = commands
        .dataset_detail(dd_ui_contracts::dataset::DatasetGetRequest {
            context: context(session),
            dataset_id: uuid(dataset_id.clone()),
        })
        .unwrap();
    assert_eq!(dataset_detail.categories[0].name, "signal");
    assert_eq!(dataset_detail.categories[0].id, 1);
    let source_id = String::from(imported.source_video_id.clone());
    let detail = |session| {
        commands
            .video_detail(VideoGetRequest {
                context: context(session),
                source_video_id: imported.source_video_id.clone(),
            })
            .unwrap()
    };
    assert!(detail(session).video_annotation_manifest.is_none());
    let wire_context = || {
        serde_json::json!({
            "request_id": Uuid::new_v4(), "workspace_id": workspace.workspace_id,
            "workspace_session_id": session,
        })
    };
    let keyframe_request = |bbox| {
        serde_json::from_value::<VideoSaveKeyframeDraftRequest>(serde_json::json!({
        "context": wire_context(), "source_video_id": source_id, "target_dataset_id": dataset_id,
        "frame_ref": frame, "expected_video_revision": detail(session).row_revision,
        "coco_document": {
            "images":[{"id":1,"file_name":"frame.png","width":48,"height":32}],
            "categories":[{"id":1,"name":"signal"}],
            "annotations":[{"id":1,"image_id":1,"category_id":1,"bbox":bbox,"area":16,"iscrowd":0}]
        }
    })).unwrap()
    };
    assert!(
        commands
            .save_video_keyframe(keyframe_request([47, 0, 4, 4]))
            .is_err()
    );
    let saved = commands
        .save_video_keyframe(keyframe_request([1, 2, 4, 4]))
        .unwrap();
    let mut excessive = keyframe_request([1, 2, 4, 4]);
    excessive.coco_document.annotations = (0..10_001)
        .map(|index| dd_ui_contracts::annotation::CocoBox {
            id: index + 1,
            image_id: 1,
            category_id: 1,
            bbox: [1.0, 2.0, 4.0, 4.0],
            area: 16.0,
            iscrowd: 0,
        })
        .collect();
    assert_eq!(
        commands.save_video_keyframe(excessive).unwrap_err().code,
        "PAYLOAD_TOO_LARGE"
    );
    let current = detail(session);
    assert_eq!(current.row_revision, saved.video_revision);
    assert!(matches!(
        saved.annotation_ref.owner,
        dd_ui_contracts::ArtifactOwner::Asset(_)
    ));
    let segment_request = |end| {
        serde_json::from_value::<VideoSaveSegmentRequest>(serde_json::json!({
        "context": wire_context(), "source_video_id": source_id, "target_dataset_id": dataset_id,
        "start_us": "600000", "end_us": end, "expected_revision": "0",
        "keyframes":[{"frame_ref": saved.frame_ref,"annotation_ref":saved.annotation_ref}]
    })).unwrap()
    };
    assert!(
        commands
            .save_video_segment(segment_request("600000"))
            .is_err()
    );
    let segment = commands
        .save_video_segment(segment_request("600001"))
        .unwrap();
    assert_eq!(String::from(segment.revision), "1");
    let reopened_segment_id = segment.segment_id;
    let sampling_key = Uuid::new_v4();
    let sample_request = |every_n, interval_ms: Option<u32>| {
        let mut context = wire_context();
        context["idempotency_key"] = serde_json::json!(sampling_key);
        serde_json::from_value::<VideoSampleSubmitRequest>(serde_json::json!({
            "context":context, "source_video_id":source_id, "segment_id":reopened_segment_id,
            "target_dataset_id":dataset_id,
            "dataset_sampling_config":{"mode":"EVERY_N_FRAMES","every_n":every_n,
                                       "interval_ms":interval_ms,"include_keyframes":true}
        }))
        .unwrap()
    };
    assert_eq!(
        commands
            .start_video_sample(Some(Arc::clone(&runner)), sample_request(1, Some(100)))
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
    let sample = commands
        .start_video_sample(Some(Arc::clone(&runner)), sample_request(1, None))
        .unwrap();
    let sample_id = sample.task_id.clone();
    let ready = wait(session, sample.task_id);
    assert_eq!(ready.status, "SUCCEEDED");
    assert!(matches!(
        ready.operation_kind,
        ImportOperationKind::VideoSample
    ));
    let receipt = ready.video_sample.unwrap();
    assert_eq!(receipt.samples.len(), 1);
    assert_eq!(String::from(receipt.samples[0].frame_index), "3");
    let staged = receipt.samples[0].asset_id.clone();
    let pending = commands
        .list_staging(dd_ui_contracts::staging::StagingListRequest {
            context: context(session),
            target_dataset_id: Some(uuid(dataset_id.clone())),
            cursor: None,
            status: None,
            page_size: 10,
        })
        .unwrap();
    assert_eq!(pending.items.len(), 1);
    let staged_view = &pending.items[0];
    assert_eq!(staged_view.asset_id, staged);
    let annotation = commands
        .annotation(dd_ui_contracts::annotation::AnnotationGetRequest {
            context: context(session),
            asset_id: staged.clone(),
        })
        .unwrap();
    assert!(matches!(
        annotation.annotation_state,
        dd_ui_contracts::dataset::AnnotationState::Unreviewed
    ));
    assert_eq!(
        annotation.formal_coco.unwrap().images[0].file_name,
        "frame-3.png"
    );
    assert!(matches!(
        staged_view.state,
        dd_ui_contracts::staging::StagingState::NeedsReview
    ));
    assert_eq!(
        staged_view
            .source_frame_ref
            .as_ref()
            .unwrap()
            .image_ref
            .artifact_id,
        frame.image_ref.artifact_id
    );
    let geometry = geometry::GeometryRunner::new(
        repo.join("target/dev/worker-venv/Scripts/python.exe"),
        repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
    );
    let checked = commands
        .start_geometry(
            geometry,
            GeometrySubmitRequest {
                context: context(session),
                dataset_id: uuid(dataset_id.clone()),
                asset_id: staged.clone(),
            },
        )
        .unwrap();
    assert_eq!(wait(session, checked.task_id.clone()).status, "SUCCEEDED");
    let deadline = Instant::now() + Duration::from_secs(5);
    let report = loop {
        let list = commands
            .list_staging(dd_ui_contracts::staging::StagingListRequest {
                context: context(session),
                target_dataset_id: Some(uuid(dataset_id.clone())),
                cursor: None,
                status: None,
                page_size: 10,
            })
            .unwrap();
        let item = list.items.into_iter().next().unwrap();
        assert!(matches!(
            item.state,
            dd_ui_contracts::staging::StagingState::NeedsReview
        ));
        if let Some(reference) = item.current_report_ref {
            break reference;
        }
        assert!(
            Instant::now() < deadline,
            "geometry completion did not publish a Staging report"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let dd_ui_contracts::ArtifactOwner::Task(report_task) = report.owner else {
        panic!("expected report Task owner");
    };
    let report_get = |scope| StagingReportGetRequest {
        context: context(scope),
        report_id: Some(report_task.clone()),
        task_id: None,
    };
    let readable = commands.get_staging_report(report_get(session)).unwrap();
    assert_eq!(readable.scope.asset_id, staged);
    assert_eq!(readable.report_artifact_ref.artifact_id, report.artifact_id);
    assert_eq!(readable.checks[0].assessment, "PASS");
    assert_eq!(readable.checks[1].assessment, "NOT_EVALUATED");
    assert_eq!(readable.unexecuted_checks, ["semantic_review"]);
    let mut ambiguous = report_get(session);
    ambiguous.task_id = ambiguous.report_id.clone();
    assert_eq!(
        commands.get_staging_report(ambiguous).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        commands
            .start_video_sample(Some(Arc::clone(&runner)), sample_request(2, None))
            .unwrap_err()
            .code,
        "CONFLICT"
    );
    let mut unconfigured = sample_request(1, None);
    unconfigured.context.idempotency_key = Some(uuid(Uuid::new_v4().to_string()));
    let failed = commands.start_video_sample(None, unconfigured).unwrap();
    assert_eq!(failed.status, "FAILED");
    assert_eq!(
        wait(session, failed.task_id).error_code.as_deref(),
        Some("WORKER_START_FAILED")
    );
    owner.close(session).unwrap();
    let response = annotation::media_response(
        &media,
        Request::builder()
            .uri(locator.locator.as_str())
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(response.status(), 403);
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        commands
            .get_staging_report(report_get(session))
            .unwrap_err()
            .code,
        "STALE_WORKSPACE_SESSION"
    );
    assert_eq!(
        commands
            .get_staging_report(report_get(reopened))
            .unwrap()
            .scope
            .asset_id,
        staged
    );
    let restored = detail(reopened);
    assert_eq!(
        wait(reopened, sample_id).video_sample.unwrap().samples[0].asset_id,
        staged
    );
    let work = restored.video_annotation_manifest.unwrap();
    assert_eq!(work.segments.len(), 1);
    assert_eq!(work.segments[0].segment_id, reopened_segment_id);
    assert!(work.unassigned_keyframe_drafts.is_empty());
    assert_eq!(
        work.segments[0].keyframes[0].annotation_ref.artifact_id,
        saved.annotation_ref.artifact_id
    );
    let draft = commands
        .get_video_keyframe(
            serde_json::from_value::<VideoGetKeyframeDraftRequest>(serde_json::json!({
                "context": {
                    "request_id": Uuid::new_v4(), "workspace_id": workspace.workspace_id,
                    "workspace_session_id": reopened,
                },
                "source_video_id": source_id,
                "annotation_ref": saved.annotation_ref,
            }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(draft.annotations[0].bbox, [1.0, 2.0, 4.0, 4.0]);
    let cached = commands
        .resolve_video_frame(
            None,
            request(reopened, serde_json::json!({"frame_index":"3"})).unwrap(),
        )
        .unwrap();
    let FrameResolution::Ready { frame: cached } = cached else {
        panic!("reopened frame should use the verified cache");
    };
    assert_eq!(cached.image_ref.artifact_id, frame.image_ref.artifact_id);
    let failed = commands
        .resolve_video_frame(
            Some(VideoRunner::new(
                run.join("absent-frame-worker.exe"),
                run.join("absent-frame-worker.py"),
            )),
            request(reopened, serde_json::json!({"frame_index":"0"})).unwrap(),
        )
        .unwrap();
    let FrameResolution::Accepted { operation } = failed else {
        panic!("uncached frame must be a task");
    };
    let failed = wait(reopened, operation.task_id);
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("WORKER_START_FAILED"));
    let absent = commands
        .resolve_video_frame(
            None,
            request(reopened, serde_json::json!({"frame_index":"1"})).unwrap(),
        )
        .unwrap();
    let FrameResolution::Accepted { operation } = absent else {
        panic!("unconfigured uncached frame must return a task");
    };
    let failed = wait(reopened, operation.task_id);
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("WORKER_START_FAILED"));
    assert_eq!(fs::read(clip).unwrap(), original);

    let dataset = uuid(dataset_id.clone());
    let current = commands
        .annotation(dd_ui_contracts::annotation::AnnotationGetRequest {
            context: context(reopened),
            asset_id: staged.clone(),
        })
        .unwrap();
    let review = commands
        .open_review(ReviewOpenRequest {
            context: context(reopened),
            dataset_id: dataset.clone(),
            asset_id: staged.clone(),
            base_image_digest: current.image_digest,
            base_annotation_digest: current.annotation_digest,
        })
        .unwrap()
        .review;
    let mut edited = current.formal_coco.unwrap();
    edited.annotations[0].bbox[0] += 1.0;
    let saved = commands
        .save_review_draft(ReviewSaveDraftRequest {
            context: context(reopened),
            review_id: review.review_id.clone(),
            source_task_id: checked.task_id,
            expected_review_revision: review.row_revision,
            expected_draft_revision: None,
            coco_document: edited,
        })
        .unwrap();
    let final_op = commands
        .commit_review(ReviewCommitRequest {
            context: context(reopened),
            review_id: review.review_id,
            expected_review_revision: saved.review_revision,
            base_image_digest: review.base_image_digest,
            base_annotation_digest: review.base_annotation_digest,
            draft_digest: saved.draft_digest,
            confirmed_negative: false,
        })
        .unwrap();
    assert_eq!(wait(reopened, final_op.task_id).status, "SUCCEEDED");
    let list = |status| {
        commands
            .list_staging(dd_ui_contracts::staging::StagingListRequest {
                context: context(reopened),
                target_dataset_id: Some(dataset.clone()),
                cursor: None,
                status,
                page_size: 10,
            })
            .unwrap()
    };
    assert!(matches!(
        list(None).items[0].state,
        StagingState::NeedsCheck
    ));
    let after_final = commands
        .start_geometry(
            geometry::GeometryRunner::new(
                repo.join("target/dev/worker-venv/Scripts/python.exe"),
                repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
            ),
            GeometrySubmitRequest {
                context: context(reopened),
                dataset_id: dataset.clone(),
                asset_id: staged.clone(),
            },
        )
        .unwrap();
    assert_eq!(wait(reopened, after_final.task_id).status, "SUCCEEDED");
    let deadline = Instant::now() + Duration::from_secs(5);
    let current_report = loop {
        if let Some(item) = list(Some(StagingState::ReadyForAcceptance))
            .items
            .into_iter()
            .next()
        {
            break item.current_report_ref.unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "the confirmed frame did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let dd_ui_contracts::ArtifactOwner::Task(task_id) = current_report.owner else {
        panic!("expected Task report");
    };
    let key = uuid(Uuid::new_v4().to_string());
    let submit = |reason: Option<&str>| {
        let report = commands
            .get_staging_report(StagingReportGetRequest {
                context: context(reopened),
                report_id: None,
                task_id: Some(task_id.clone()),
            })
            .unwrap();
        let mut command_context = context(reopened);
        command_context.idempotency_key = Some(key.clone());
        StagingCommitRequest {
            context: command_context,
            target_dataset_id: dataset.clone(),
            asset_ids: vec![staged.clone()],
            quality_report_ref: report.report_artifact_ref,
            expected_schema_fingerprint: report.scope.label_schema_digest,
            acknowledged_findings: vec![],
            acknowledged_incomplete_checks: reason.map(|reason| {
                vec![StagingIncompleteCheckAck {
                    check_id: "semantic_review".into(),
                    reason: reason.into(),
                }]
            }),
        }
    };
    assert_eq!(
        commands.commit_staging(submit(None)).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    let reason = "Reviewed sample locally";
    let accepted = commands.commit_staging(submit(Some(reason))).unwrap();
    let result = wait(reopened, accepted.task_id.clone());
    assert_eq!(result.status, "SUCCEEDED");
    assert!(matches!(
        result.operation_kind,
        ImportOperationKind::StagingCommit
    ));
    let receipt = result.staging_commit.unwrap();
    assert_eq!(receipt.accepted_asset_ids, [staged.clone()]);
    assert_eq!(receipt.acknowledged_incomplete_checks[0].reason, reason);
    assert!(list(None).items.is_empty());
    let members = owner
        .list_assets(reopened, Uuid::parse_str(&dataset_id).unwrap(), None, 10)
        .unwrap();
    assert!(
        members
            .items
            .iter()
            .any(|item| item.asset_id.to_string() == String::from(staged.clone()))
    );
    assert_eq!(
        commands
            .commit_staging(submit(Some(reason)))
            .unwrap()
            .task_id,
        accepted.task_id
    );
}
