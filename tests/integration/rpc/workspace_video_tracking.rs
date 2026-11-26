use std::{fs, sync::Arc};

use dd_infra::frame_runtime::{
    FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget, SeekPolicy,
};
use dd_infra::video_annotation_store::{VideoFrameReference, VideoKeyframe};
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationService, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use dd_infra::video_tracking_runtime::{
    TRANSLATION_PROFILE_ID, VideoTrackingRuntime, VideoTrackingService, VideoTrackingSubmission,
};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/workspace_video.rs"]
mod workspace_video;
use workspace_video::Fixture;

#[path = "support/tracking_video_fixture.rs"]
mod tracking_video_fixture;

#[tokio::test]
async fn a_tracking_task_uses_a_frozen_workspace_plan_and_reads_the_sealed_candidates_after_reopen()
{
    let fixture = Fixture::create_with(
        "Workspace tracking.mkv",
        tracking_video_fixture::make_motion_clip,
    )
    .await;
    let root = &fixture.root;
    let owner = &fixture.owner;
    let video = fixture.video_id;
    let session = fixture.session;
    let dataset = Uuid::new_v4();
    let connection = Connection::open(root.join("workspace.db")).unwrap();
    connection
        .execute(
            r#"INSERT INTO datasets(dataset_id,name,categories_json,created_at_ms,updated_at_ms)
         VALUES (?1,'Tracked','[{"id":3,"name":"signal"}]',100,100)"#,
            [dataset.to_string()],
        )
        .unwrap();
    let resolve = FrameService::new(Arc::clone(owner)).unwrap();
    let (frame_task, job) = resolve
        .submit(
            session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                video_id: video,
                target: FrameTarget::Index(0),
                policy: SeekPolicy::Nearest,
            },
            230,
        )
        .unwrap();
    let mut frame_worker = FrameResolveRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        frame_worker.run(job.unwrap()).await.unwrap().status,
        "SUCCEEDED"
    );
    frame_worker.shutdown().await.unwrap();
    let frame = resolve
        .snapshot(session, frame_task.task_id)
        .unwrap()
        .frame
        .unwrap();
    let reference = VideoFrameReference::from_resolved(frame_task.task_id, &frame).unwrap();
    let work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
    let draft = work.save(session,KeyframeDraftRequest {
        source_video_id:video, target_dataset_id:dataset, frame_ref:reference.clone(),
        expected_video_revision:0,
        coco_document:serde_json::to_vec(&json!({
            "images":[{"id":1,"file_name":"frame.png","width":34,"height":26}],
            "categories":[{"id":3,"name":"signal"}],
            "annotations":[{"id":7,"image_id":1,"category_id":3,"bbox":[8,8,5,5],"area":25,"iscrowd":0}],
        })).unwrap(),
    },231).unwrap();
    let head = VideoKeyframe {
        frame_ref: reference,
        annotation_ref: draft.annotation_ref,
    };
    let segment = work
        .save_segment(
            session,
            SegmentRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                segment_id: None,
                start_us: 0,
                end_us: 600_000,
                keyframes: vec![head.clone()],
                expected_revision: 0,
            },
            232,
        )
        .unwrap();
    let service = VideoTrackingService::new(Arc::clone(owner)).unwrap();
    let request = VideoTrackingSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        tracker_profile_id: TRANSLATION_PROFILE_ID,
        selection: VideoSamplingRequest {
            source_video_id: video,
            segment_id: segment.segment_id,
            target_dataset_id: dataset,
            mode: VideoSamplingMode::EveryNFrames { every_n: 2 },
            purpose: VideoSamplingPurpose::Tracking {
                supports_time_interval: true,
            },
        },
    };
    let (queued, job) = service.submit(session, request, 233).unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert!(queued.bundle.is_none());
    let replay = service.submit(session, request, 234).unwrap();
    assert_eq!(replay.0.task_id, queued.task_id);
    assert!(replay.1.is_none());
    let changed = VideoTrackingSubmission {
        selection: VideoSamplingRequest {
            mode: VideoSamplingMode::EveryNFrames { every_n: 1 },
            ..request.selection
        },
        ..request
    };
    assert!(service.submit(session, changed, 234).is_err());

    work.save_segment(
        session,
        SegmentRequest {
            source_video_id: video,
            target_dataset_id: dataset,
            segment_id: Some(segment.segment_id),
            start_us: 0,
            end_us: 250_000,
            keyframes: vec![head],
            expected_revision: segment.revision,
        },
        235,
    )
    .unwrap();
    let mut worker =
        VideoTrackingRuntime::launch(Arc::clone(owner), session, fixture.python, fixture.entry)
            .await
            .unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    let finished = service.snapshot(session, queued.task_id).unwrap();
    assert_eq!(finished.status, "SUCCEEDED");
    let bundle = finished.bundle.unwrap();
    assert_eq!(bundle.data.processed_frame_count, 3);
    assert_eq!(bundle.data.source_video_id, video);
    assert_eq!(bundle.data.segment_id, segment.segment_id);
    assert_eq!(bundle.data.observations[2].frame_index, "4");
    assert_eq!(
        bundle.data.observations[2].bbox_xywh,
        [15.0, 11.0, 5.0, 5.0]
    );
    assert_eq!(
        bundle.data.observations[0].track_id,
        bundle.data.observations[2].track_id
    );
    assert_eq!(bundle.data.observations[2].category_id, "3");
    assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
    let next_request = || VideoTrackingSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        ..request
    };
    let (unreadable, job) = service.submit(session, next_request(), 236).unwrap();
    let manifest: String = connection
        .query_row(
            "SELECT input_manifest_path FROM tasks WHERE task_id=?1",
            [unreadable.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    fs::write(root.join(manifest), b"changed input manifest").unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "FAILED");
    let failed = service.snapshot(session, unreadable.task_id).unwrap();
    assert_eq!(failed.error_code.as_deref(), Some("INPUT_MANIFEST_INVALID"));
    assert!(failed.bundle.is_none());
    let (changed_source, job) = service.submit(session, next_request(), 237).unwrap();
    fs::write(&fixture.source, b"changed synthetic video").unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "FAILED");
    let changed = service.snapshot(session, changed_source.task_id).unwrap();
    assert_eq!(changed.error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(changed.bundle.is_none());
    fs::write(&fixture.source, &fixture.original).unwrap();
    let (pending, _) = service.submit(session, next_request(), 238).unwrap();
    worker.shutdown().await.unwrap();
    let output: String = connection
        .query_row(
            "SELECT output_manifest_path FROM tasks WHERE task_id=?1",
            [queued.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM staging_items", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(connection);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(service.snapshot(session, queued.task_id).is_err());
    let again = service
        .snapshot(reopened, queued.task_id)
        .unwrap()
        .bundle
        .unwrap();
    assert_eq!(again.reference, bundle.reference);
    let interrupted = service.snapshot(reopened, pending.task_id).unwrap();
    assert_eq!(interrupted.status, "INTERRUPTED");
    assert_eq!(interrupted.error_code.as_deref(), Some("SESSION_ENDED"));
    let evidence = root.join(output).with_file_name("video-tracks.json");
    fs::write(evidence, b"tampered").unwrap();
    assert!(service.snapshot(reopened, queued.task_id).is_err());
}
