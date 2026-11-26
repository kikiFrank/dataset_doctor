use std::{fs, sync::Arc};

use dd_infra::video_keyframes::{
    SegmentRequest, VideoAnnotationService, VideoSampleService, VideoSampleSubmission,
    VideoSamplingMode, VideoSamplingPurpose, VideoSamplingRequest,
};
use dd_infra::video_tracking_runtime::{VideoTrackingRuntime, VideoTrackingService};
use rusqlite::Connection;
use serde_json::Value;
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/tracking_workspace_fixture.rs"]
mod tracking_workspace_fixture;
use tracking_workspace_fixture::TrackingWorkspace;

#[tokio::test]
async fn a_tracking_receipt_supplies_only_exact_candidate_frames_to_training_staging() {
    let setup = TrackingWorkspace::create().await;
    let fixture = &setup.fixture;
    let owner = &fixture.owner;
    let session = fixture.session;
    let tracker = VideoTrackingService::new(Arc::clone(owner)).unwrap();
    let (task, job) = tracker
        .submit(session, setup.tracking_request(), 233)
        .unwrap();
    let mut worker = VideoTrackingRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    worker.shutdown().await.unwrap();
    let bundle = tracker
        .snapshot(session, task.task_id)
        .unwrap()
        .bundle
        .unwrap();
    let sampler = VideoSampleService::new(Arc::clone(owner)).unwrap();
    let submit = || VideoSampleSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        selection: VideoSamplingRequest {
            source_video_id: fixture.video_id,
            target_dataset_id: setup.dataset,
            segment_id: setup.segment.segment_id,
            mode: VideoSamplingMode::EveryNFrames { every_n: 1 },
            purpose: VideoSamplingPurpose::Dataset {
                include_keyframes: true,
                max_frames: Some(6),
            },
        },
    };
    let plain = submit();
    let (plain_task, job) = sampler.submit(session, plain, 234).unwrap();
    sampler
        .run(
            session,
            job.unwrap(),
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    let frames = sampler
        .snapshot(session, plain_task.task_id, 235)
        .unwrap()
        .unwrap()
        .receipt
        .unwrap()
        .samples;
    assert_eq!(frames.len(), 6);
    assert!(
        sampler
            .submit_with_tracking(session, plain, bundle.reference.clone(), 236)
            .is_err()
    );

    let tracked_request = submit();
    let (sampled, job) = sampler
        .submit_with_tracking(session, tracked_request, bundle.reference.clone(), 237)
        .unwrap();
    sampler
        .run(
            session,
            job.unwrap(),
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    let snap = sampler
        .snapshot(session, sampled.task_id, 238)
        .unwrap()
        .unwrap();
    assert_eq!(snap.status, "SUCCEEDED", "{:?}", snap.error_code);
    let samples = snap.receipt.unwrap().samples;
    assert_eq!(samples, frames);
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    for sample in &samples {
        let (annotation, source): (Option<String>, String) = connection
            .query_row(
                "SELECT annotation_path,source_metadata_json FROM assets WHERE asset_id=?1",
                [sample.asset_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(annotation.is_some(), sample.frame_index == 0);
        let source: Value = serde_json::from_str(&source).unwrap();
        let candidate = &source["tracking_candidate_ref"];
        if matches!(sample.frame_index, 2 | 4) {
            assert_eq!(
                candidate["artifact_id"],
                bundle.reference.artifact_id.to_string()
            );
            assert_eq!(candidate["sha256"], bundle.reference.sha256);
            assert_eq!(
                source["frame"]["frame_index"],
                sample.frame_index.to_string()
            );
        } else {
            assert!(candidate.is_null());
        }
    }
    let work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
    for sample in &samples {
        let candidates = work
            .staging_tracking_candidates(session, setup.dataset, sample.asset_id)
            .unwrap();
        if matches!(sample.frame_index, 2 | 4) {
            let candidates = candidates.unwrap();
            assert_eq!(candidates.reference, bundle.reference);
            assert_eq!(candidates.frame_index, sample.frame_index);
            let expected = bundle
                .data
                .observations
                .iter()
                .find(|item| {
                    item.frame_index == sample.frame_index.to_string() && item.source == "TRACKER"
                })
                .unwrap();
            assert_eq!(candidates.observations.len(), 1);
            assert_eq!(candidates.observations[0].track_id, expected.track_id);
            assert_eq!(candidates.observations[0].bbox_xywh, expected.bbox_xywh);
        } else {
            assert!(candidates.is_none());
        }
    }
    let tracked_frame = samples[2].asset_id;
    let original_index = samples[2].frame_index.to_string();
    assert!(
        work.staging_tracking_candidates(Uuid::new_v4(), setup.dataset, tracked_frame)
            .is_err()
    );
    assert!(
        work.staging_tracking_candidates(session, Uuid::new_v4(), tracked_frame)
            .unwrap()
            .is_none()
    );
    let update_frame = |frame: &str| {
        connection.execute(
            "UPDATE assets SET source_metadata_json=json_set(source_metadata_json,'$.frame.frame_index',?2)
             WHERE asset_id=?1",
            rusqlite::params![tracked_frame.to_string(), frame],
        ).unwrap();
    };
    update_frame("4");
    assert!(
        work.staging_tracking_candidates(session, setup.dataset, tracked_frame)
            .is_err(),
        "another valid candidate frame must not be read for this asset"
    );
    update_frame(&original_index);
    let stored_path: String = connection
        .query_row(
            "SELECT path FROM assets WHERE asset_id=?1",
            [tracked_frame.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let managed = fixture.root.join(stored_path);
    let png = fs::read(&managed).unwrap();
    fs::write(&managed, b"changed managed frame").unwrap();
    assert!(
        work.staging_tracking_candidates(session, setup.dataset, tracked_frame)
            .is_err()
    );
    fs::write(&managed, png).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dataset_assets", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sampler
            .submit_with_tracking(session, tracked_request, bundle.reference.clone(), 239)
            .unwrap()
            .0
            .task_id,
        sampled.task_id
    );
    let frozen_path: String = connection
        .query_row(
            "SELECT input_manifest_path FROM tasks WHERE task_id=?1",
            [sampled.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let frozen: Value =
        serde_json::from_slice(&fs::read(fixture.root.join(frozen_path)).unwrap()).unwrap();
    assert_eq!(
        frozen["resolved_config"]["selection"]["tracking_result_ref"]["sha256"],
        bundle.reference.sha256
    );

    let (tampered, job) = sampler
        .submit_with_tracking(session, submit(), bundle.reference.clone(), 239)
        .unwrap();
    let worker_output: String = connection
        .query_row(
            "SELECT output_manifest_path FROM tasks WHERE task_id=?1",
            [task.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let candidate = fixture
        .root
        .join(worker_output)
        .with_file_name("video-tracks.json");
    let original = fs::read(&candidate).unwrap();
    fs::write(&candidate, b"changed after samples were admitted").unwrap();
    assert!(
        work.staging_tracking_candidates(session, setup.dataset, tracked_frame)
            .is_err()
    );
    sampler
        .run(
            session,
            job.unwrap(),
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
    let failed = sampler
        .snapshot(session, tampered.task_id, 240)
        .unwrap()
        .unwrap();
    assert_eq!(
        failed.error_code.as_deref(),
        Some("TRACKING_EVIDENCE_INVALID")
    );
    assert!(failed.receipt.is_none());
    fs::write(candidate, original).unwrap();
    assert!(
        work.staging_tracking_candidates(session, setup.dataset, tracked_frame)
            .unwrap()
            .is_some()
    );

    work.save_segment(
        session,
        SegmentRequest {
            source_video_id: fixture.video_id,
            target_dataset_id: setup.dataset,
            segment_id: Some(setup.segment.segment_id),
            start_us: 0,
            end_us: 250_000,
            keyframes: vec![setup.head],
            expected_revision: setup.segment.revision,
        },
        240,
    )
    .unwrap();
    assert!(
        sampler
            .submit_with_tracking(session, submit(), bundle.reference, 241)
            .is_err()
    );
    assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
}
