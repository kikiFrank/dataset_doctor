use std::{fs, sync::Arc};

use dd_infra::video_annotation_store::VideoKeyframe;
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationService, VideoSampleService,
    VideoSampleSubmission, VideoSamplingMode, VideoSamplingPurpose, VideoSamplingRequest,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

#[tokio::test]
async fn durable_sample_task_uses_frozen_frames_and_publishes_staging_with_its_receipt() {
    let (fixture, dataset, frame_ref) = prepared().await;
    let annotation = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let revision = annotation
        .read(fixture.session, fixture.video_id)
        .unwrap()
        .video_revision;
    let draft = annotation
        .save(
            fixture.session,
            KeyframeDraftRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                frame_ref: frame_ref.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: revision,
            },
            299,
        )
        .unwrap();
    let segment = SegmentRequest {
        source_video_id: fixture.video_id,
        target_dataset_id: dataset,
        segment_id: None,
        start_us: 0,
        end_us: 700_000,
        keyframes: vec![VideoKeyframe {
            frame_ref,
            annotation_ref: draft.annotation_ref,
        }],
        expected_revision: 0,
    };
    let stored = annotation
        .save_segment(fixture.session, segment.clone(), 300)
        .unwrap();
    let submission = VideoSampleSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        selection: VideoSamplingRequest {
            source_video_id: fixture.video_id,
            segment_id: stored.segment_id,
            target_dataset_id: dataset,
            mode: VideoSamplingMode::EveryNFrames { every_n: 2 },
            purpose: VideoSamplingPurpose::Dataset {
                include_keyframes: true,
                max_frames: Some(3),
            },
        },
    };
    let service = VideoSampleService::new(Arc::clone(&fixture.owner)).unwrap();
    let (queued, job) = service.submit(fixture.session, submission, 301).unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert!(queued.receipt.is_none());
    let (replay, unexpected) = service.submit(fixture.session, submission, 302).unwrap();
    assert_eq!(replay.task_id, queued.task_id);
    assert!(unexpected.is_none());
    let mut changed = submission;
    changed.selection.mode = VideoSamplingMode::EveryNFrames { every_n: 3 };
    assert!(service.submit(fixture.session, changed, 302).is_err());
    annotation
        .save_segment(
            fixture.session,
            SegmentRequest {
                segment_id: Some(stored.segment_id),
                start_us: 400_000,
                expected_revision: 1,
                ..segment
            },
            303,
        )
        .unwrap();
    service
        .run(
            fixture.session,
            job.unwrap(),
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    let complete = service
        .snapshot(fixture.session, queued.task_id, 400)
        .unwrap()
        .unwrap();
    assert_eq!(complete.status, "SUCCEEDED");
    let receipt = complete.receipt.unwrap();
    assert_eq!(receipt.segment_revision, 1);
    assert_eq!(
        receipt
            .samples
            .iter()
            .map(|sample| sample.frame_index)
            .collect::<Vec<_>>(),
        [0, 2, 3]
    );
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let rows = connection
        .query_row(
            "SELECT
        (SELECT count(*) FROM staging_items),(SELECT count(*) FROM dataset_assets)",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(rows, (3, 0));
    drop(connection);
    let pending = VideoSampleSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        ..submission
    };
    let (accepted, _) = service.submit(fixture.session, pending, 401).unwrap();
    fixture.owner.close(fixture.session).unwrap();
    let grant = fixture
        .owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = fixture
        .owner
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        service
            .snapshot(reopened, queued.task_id, 402)
            .unwrap()
            .unwrap()
            .receipt
            .unwrap(),
        receipt
    );
    assert_eq!(
        service
            .snapshot(reopened, accepted.task_id, 402)
            .unwrap()
            .unwrap()
            .status,
        "INTERRUPTED"
    );
    let (replayed, next) = service.submit(reopened, submission, 403).unwrap();
    assert_eq!(replayed.task_id, queued.task_id);
    assert!(next.is_none());
    assert_eq!(fs::read(fixture.source).unwrap(), fixture.original);
}

#[tokio::test]
async fn sample_failure_and_missing_worker_leave_no_staging_rows() {
    let (fixture, dataset, _) = prepared().await;
    let annotation = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let saved = annotation
        .save_segment(
            fixture.session,
            SegmentRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                segment_id: None,
                start_us: 0,
                end_us: 700_000,
                keyframes: vec![],
                expected_revision: 0,
            },
            300,
        )
        .unwrap();
    let submission = VideoSampleSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        selection: VideoSamplingRequest {
            source_video_id: fixture.video_id,
            segment_id: saved.segment_id,
            target_dataset_id: dataset,
            mode: VideoSamplingMode::EveryNFrames { every_n: 2 },
            purpose: VideoSamplingPurpose::Dataset {
                include_keyframes: false,
                max_frames: None,
            },
        },
    };
    let service = VideoSampleService::new(Arc::clone(&fixture.owner)).unwrap();
    let (queued, job) = service.submit(fixture.session, submission, 301).unwrap();
    service
        .run(
            fixture.session,
            job.unwrap(),
            fixture.root.join("absent-worker.exe"),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    let failure = service
        .snapshot(fixture.session, queued.task_id, 400)
        .unwrap()
        .unwrap();
    assert_eq!(failure.status, "FAILED");
    assert_eq!(failure.error_code.as_deref(), Some("WORKER_START_FAILED"));
    assert!(failure.receipt.is_none());
    let (queued, job) = service
        .submit(
            fixture.session,
            VideoSampleSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                ..submission
            },
            401,
        )
        .unwrap();
    fs::write(&fixture.source, b"changed since sample admission").unwrap();
    service
        .run(
            fixture.session,
            job.unwrap(),
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    let failure = service
        .snapshot(fixture.session, queued.task_id, 402)
        .unwrap()
        .unwrap();
    assert_eq!(failure.status, "FAILED");
    assert_eq!(failure.error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(failure.receipt.is_none());
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM staging_items", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    fs::write(&fixture.source, &fixture.original).unwrap();
    let (queued, job) = service
        .submit(
            fixture.session,
            VideoSampleSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                ..submission
            },
            403,
        )
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_sample_receipt BEFORE UPDATE OF status ON tasks
        WHEN NEW.operation_kind='VIDEO_SAMPLE' AND NEW.status='SUCCEEDED'
        BEGIN SELECT RAISE(ABORT,'fixture terminal write failure'); END;",
        )
        .unwrap();
    service
        .run(
            fixture.session,
            job.unwrap(),
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .snapshot(fixture.session, queued.task_id, 404)
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some("STAGING_PUBLICATION_FAILED")
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM staging_items", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection
        .execute_batch("DROP TRIGGER reject_sample_receipt;")
        .unwrap();
    let (queued, job) = service
        .submit(
            fixture.session,
            VideoSampleSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                ..submission
            },
            405,
        )
        .unwrap();
    let input: String = connection
        .query_row(
            "SELECT input_manifest_path FROM tasks WHERE task_id=?1",
            [queued.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    fs::write(fixture.root.join(input), b"changed sample manifest").unwrap();
    service
        .run(fixture.session, job.unwrap(), fixture.python, fixture.entry)
        .await
        .unwrap();
    assert_eq!(
        service
            .snapshot(fixture.session, queued.task_id, 406)
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some("INPUT_MANIFEST_INVALID")
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM staging_items", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
