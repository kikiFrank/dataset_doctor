use std::{fs, process::Command, sync::Arc};

use dd_domain::video::SeekPolicy;
use dd_infra::frame_runtime::{FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget};
use dd_infra::video_annotation_store::{VideoArtifactOwner, VideoKeyframe};
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationService, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use rusqlite::Connection;
use serde_json::Value;
use uuid::Uuid;

#[path = "support/video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

#[tokio::test]
async fn verified_frames_enter_staging_once_and_only_the_manual_frame_has_labels() {
    let (fixture, dataset, frame_ref) = prepared().await;
    let VideoArtifactOwner::Task(blue_task) = frame_ref.image_ref.owner else {
        panic!("the saved frame was not resolved by a task");
    };
    let service = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let initial = service.read(fixture.session, fixture.video_id).unwrap();
    let draft = service
        .save(
            fixture.session,
            KeyframeDraftRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                frame_ref: frame_ref.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: initial.video_revision,
            },
            300,
        )
        .unwrap();
    let segment = service
        .save_segment(
            fixture.session,
            SegmentRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                segment_id: None,
                start_us: 0,
                end_us: 800_000,
                keyframes: vec![VideoKeyframe {
                    frame_ref,
                    annotation_ref: draft.annotation_ref,
                }],
                expected_revision: 0,
            },
            301,
        )
        .unwrap();
    let plan = service
        .resolve_sampling(
            fixture.session,
            VideoSamplingRequest {
                source_video_id: fixture.video_id,
                segment_id: segment.segment_id,
                target_dataset_id: dataset,
                mode: VideoSamplingMode::EveryNFrames { every_n: 3 },
                purpose: VideoSamplingPurpose::Dataset {
                    include_keyframes: true,
                    max_frames: Some(2),
                },
            },
        )
        .unwrap();
    let frames = FrameService::new(Arc::clone(&fixture.owner)).unwrap();
    let (red, pending) = frames
        .submit(
            fixture.session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                video_id: fixture.video_id,
                target: FrameTarget::Index(0),
                policy: SeekPolicy::AtOrAfter,
            },
            302,
        )
        .unwrap();
    let mut worker = FrameResolveRuntime::launch(
        Arc::clone(&fixture.owner),
        fixture.session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        worker.run(pending.unwrap()).await.unwrap().status,
        "SUCCEEDED"
    );
    worker.shutdown().await.unwrap();
    let evidence = [red.task_id, blue_task];
    assert!(
        service
            .stage_dataset_samples(fixture.session, &plan, &[blue_task, red.task_id], 303)
            .is_err()
    );
    assert!(
        service
            .stage_dataset_samples(fixture.session, &plan, &[red.task_id], 303)
            .is_err()
    );
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let counts = || {
        connection.query_row(
        "SELECT (SELECT count(*) FROM staging_items), (SELECT count(*) FROM dataset_assets),
                (SELECT count(*) FROM assets WHERE file_origin='GENERATED')", [],
        |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?)),
    ).unwrap()
    };
    assert_eq!(counts(), (0, 0, 0));
    let artifact = frames
        .snapshot(fixture.session, red.task_id)
        .unwrap()
        .frame
        .unwrap();
    let cached_path = fixture.root.join(artifact.relative_path);
    let original_frame = fs::read(&cached_path).unwrap();
    fs::write(&cached_path, b"corrupted decoder result").unwrap();
    assert!(
        service
            .stage_dataset_samples(fixture.session, &plan, &evidence, 303)
            .is_err()
    );
    assert_eq!(counts(), (0, 0, 0));
    fs::write(&cached_path, &original_frame).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_blue BEFORE INSERT ON staging_items
        WHEN (SELECT count(*) FROM staging_items)=1 BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    assert!(
        service
            .stage_dataset_samples(fixture.session, &plan, &evidence, 304)
            .is_err()
    );
    assert_eq!(
        counts(),
        (0, 0, 0),
        "both assets and staging rows roll back together"
    );
    connection
        .execute_batch("DROP TRIGGER reject_blue;")
        .unwrap();
    let staged = service
        .stage_dataset_samples(fixture.session, &plan, &evidence, 305)
        .unwrap();
    assert_eq!(
        staged
            .iter()
            .map(|item| item.frame_index)
            .collect::<Vec<_>>(),
        [0, 3]
    );
    assert_eq!(counts(), (2, 0, 2));
    let replay = service
        .stage_dataset_samples(fixture.session, &plan, &evidence, 306)
        .unwrap();
    assert_eq!(staged, replay);
    let overlapping = service
        .resolve_sampling(fixture.session, plan.request)
        .unwrap();
    assert_ne!(plan.sampling_config_id, overlapping.sampling_config_id);
    assert_eq!(
        service
            .stage_dataset_samples(fixture.session, &overlapping, &evidence, 306)
            .unwrap(),
        staged
    );
    let source = |asset: Uuid| {
        let (path, annotation, origin, metadata) = connection
            .query_row(
                "SELECT path, annotation_path, annotation_origin, source_metadata_json
             FROM assets WHERE asset_id=?1 AND source_video_id=?2 AND location_kind='WORKSPACE'",
                [asset.to_string(), fixture.video_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .unwrap();
        (
            fixture.root.join(path),
            annotation.map(|path| fixture.root.join(path)),
            origin,
            serde_json::from_str::<Value>(&metadata).unwrap(),
        )
    };
    let (red_path, red_annotation, origin, metadata) = source(staged[0].asset_id);
    assert_eq!(origin, "NONE");
    assert!(red_annotation.is_none());
    assert_eq!(metadata["kind"], "video_frame");
    assert_eq!(metadata["frame"]["frame_index"], "0");
    assert_eq!(metadata["frame"]["pts_us"], "0");
    assert!(metadata["frame"]["time_base_den"].as_u64().unwrap() > 0);
    assert_eq!(
        metadata["sampling"]["dataset"]["sampling_config_id"],
        plan.sampling_config_id.to_string()
    );
    assert_eq!(metadata["extraction_task_id"], red.task_id.to_string());
    assert!(Uuid::parse_str(metadata["extraction_attempt_id"].as_str().unwrap()).is_ok());
    let (blue_path, annotation, origin, _) = source(staged[1].asset_id);
    assert_eq!(origin, "APP_CREATED");
    let coco: Value = serde_json::from_slice(&fs::read(annotation.unwrap()).unwrap()).unwrap();
    assert_eq!(
        coco["images"][0]["file_name"],
        blue_path.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(
        coco["annotations"][0]["bbox"],
        serde_json::json!([2, 3, 10, 8])
    );
    let mut pixels = Command::new(&fixture.python);
    pixels.args(["-I", "-B", "-c", "from PIL import Image;import sys; a,b=(Image.open(p) for p in sys.argv[1:]); r,g,bl=a.getpixel((20,12)); assert r>240 and max(g,bl)<12; r,g,bl=b.getpixel((20,12)); assert bl>240 and max(r,g)<12"]).arg(red_path).arg(blue_path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        pixels.creation_flags(0x08000000);
    }
    assert!(pixels.status().unwrap().success());
    assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
    fixture.owner.close(fixture.session).unwrap();
    let grant = fixture
        .owner
        .grant_selected_directory(fixture.root)
        .unwrap();
    let reopened = fixture
        .owner
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        service
            .stage_dataset_samples(reopened, &plan, &evidence, 307)
            .unwrap(),
        staged
    );
    assert_eq!(counts(), (2, 0, 2));
}
