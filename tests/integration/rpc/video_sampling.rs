use std::{fs, sync::Arc};

use dd_infra::video_annotation_store::VideoKeyframe;
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationService, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

#[tokio::test]
async fn saved_segments_select_distinct_real_frames_for_tracking_and_training() {
    let (fixture, dataset, frame_ref) = prepared().await;
    let service = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let version = service
        .read(fixture.session, fixture.video_id)
        .unwrap()
        .video_revision;
    let saved = service
        .save(
            fixture.session,
            KeyframeDraftRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                frame_ref: frame_ref.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: version,
            },
            300,
        )
        .unwrap();
    let keyframe = VideoKeyframe {
        frame_ref,
        annotation_ref: saved.annotation_ref,
    };
    let segment = SegmentRequest {
        source_video_id: fixture.video_id,
        target_dataset_id: dataset,
        segment_id: None,
        start_us: 100_000,
        end_us: 700_000,
        keyframes: vec![keyframe.clone()],
        expected_revision: 0,
    };
    let saved = service
        .save_segment(fixture.session, segment.clone(), 301)
        .unwrap();
    let request = VideoSamplingRequest {
        source_video_id: fixture.video_id,
        segment_id: saved.segment_id,
        target_dataset_id: dataset,
        mode: VideoSamplingMode::EveryNFrames { every_n: 3 },
        purpose: VideoSamplingPurpose::Dataset {
            include_keyframes: false,
            max_frames: None,
        },
    };
    let indices = |frames: &[dd_domain::video::FrameRef]| {
        frames.iter().map(|f| f.frame_index).collect::<Vec<_>>()
    };
    let training = service.resolve_sampling(fixture.session, request).unwrap();
    assert_eq!(indices(&training.frames), [1]);
    assert!(training.keyframes.is_empty());
    assert_eq!(
        training.video_annotation_manifest_ref,
        saved.video_annotation_manifest_ref
    );
    let tracking = service
        .resolve_sampling(
            fixture.session,
            VideoSamplingRequest {
                purpose: VideoSamplingPurpose::Tracking {
                    supports_time_interval: false,
                },
                ..request
            },
        )
        .unwrap();
    assert_eq!(indices(&tracking.frames), [1, 3]);
    assert_eq!(tracking.keyframes, [keyframe.clone()]);
    assert_ne!(training.sampling_config_id, tracking.sampling_config_id);
    let dense = service
        .resolve_sampling(
            fixture.session,
            VideoSamplingRequest {
                mode: VideoSamplingMode::TimeInterval { interval_ms: 25 },
                purpose: VideoSamplingPurpose::Dataset {
                    include_keyframes: true,
                    max_frames: Some(3),
                },
                ..request
            },
        )
        .unwrap();
    assert_eq!(indices(&dense.frames), [1, 2, 3]);
    assert_eq!(dense.keyframes, [keyframe]);
    assert_eq!(dense.frames[0].pts_us, Some(200_000));
    assert_eq!(dense.frames[2].pts_us, Some(600_000));
    assert_eq!(
        service
            .read(fixture.session, fixture.video_id)
            .unwrap()
            .video_revision,
        version + 2
    );
    assert!(
        service
            .resolve_sampling(
                fixture.session,
                VideoSamplingRequest {
                    purpose: VideoSamplingPurpose::Dataset {
                        include_keyframes: true,
                        max_frames: Some(1)
                    },
                    ..request
                }
            )
            .is_err()
    );
    assert!(
        service
            .resolve_sampling(
                fixture.session,
                VideoSamplingRequest {
                    mode: VideoSamplingMode::TimeInterval { interval_ms: 200 },
                    purpose: VideoSamplingPurpose::Tracking {
                        supports_time_interval: false
                    },
                    ..request
                }
            )
            .is_err()
    );
    service
        .save_segment(
            fixture.session,
            SegmentRequest {
                segment_id: Some(saved.segment_id),
                end_us: 600_000,
                keyframes: vec![],
                expected_revision: 1,
                ..segment
            },
            302,
        )
        .unwrap();
    assert_eq!(dense.segment_revision, 1);
    assert_eq!(indices(&dense.frames), [1, 2, 3]);
    let updated = service
        .resolve_sampling(
            fixture.session,
            VideoSamplingRequest {
                mode: VideoSamplingMode::EveryNFrames { every_n: 1 },
                ..request
            },
        )
        .unwrap();
    assert_eq!(updated.segment_revision, 2);
    assert_eq!(indices(&updated.frames), [1, 2]);
}

#[tokio::test]
async fn sampling_rejects_wrong_scope_changed_source_and_stale_categories() {
    let (fixture, dataset, _) = prepared().await;
    let service = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let saved = service
        .save_segment(
            fixture.session,
            SegmentRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                segment_id: None,
                start_us: 0,
                end_us: 600_000,
                keyframes: vec![],
                expected_revision: 0,
            },
            300,
        )
        .unwrap();
    let request = VideoSamplingRequest {
        source_video_id: fixture.video_id,
        segment_id: saved.segment_id,
        target_dataset_id: dataset,
        mode: VideoSamplingMode::EveryNFrames { every_n: 1 },
        purpose: VideoSamplingPurpose::Dataset {
            include_keyframes: false,
            max_frames: None,
        },
    };
    assert!(service.resolve_sampling(fixture.session, request).is_ok());
    assert!(service.resolve_sampling(Uuid::new_v4(), request).is_err());
    assert!(
        service
            .resolve_sampling(
                fixture.session,
                VideoSamplingRequest {
                    target_dataset_id: Uuid::new_v4(),
                    ..request
                }
            )
            .is_err()
    );
    assert!(
        service
            .resolve_sampling(
                fixture.session,
                VideoSamplingRequest {
                    mode: VideoSamplingMode::EveryNFrames { every_n: 0 },
                    ..request
                }
            )
            .is_err()
    );
    assert!(
        service
            .resolve_sampling(
                fixture.session,
                VideoSamplingRequest {
                    purpose: VideoSamplingPurpose::Tracking {
                        supports_time_interval: true
                    },
                    ..request
                }
            )
            .is_err(),
        "tracking needs at least one saved manual keyframe"
    );
    fs::write(&fixture.source, b"changed synthetic source").unwrap();
    assert!(service.resolve_sampling(fixture.session, request).is_err());
    fs::write(&fixture.source, &fixture.original).unwrap();
    Connection::open(fixture.root.join("workspace.db"))
        .unwrap()
        .execute(
            "UPDATE datasets SET categories_json='[]' WHERE dataset_id=?1",
            [dataset.to_string()],
        )
        .unwrap();
    assert!(service.resolve_sampling(fixture.session, request).is_err());
}
