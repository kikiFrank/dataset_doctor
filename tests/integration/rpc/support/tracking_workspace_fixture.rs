use std::sync::Arc;

use dd_domain::video::SeekPolicy;
use dd_infra::frame_runtime::{FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget};
use dd_infra::video_annotation_store::{VideoFrameReference, VideoKeyframe};
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentReceipt, SegmentRequest, VideoAnnotationService,
    VideoSamplingMode, VideoSamplingPurpose, VideoSamplingRequest,
};
use dd_infra::video_tracking_runtime::{TRANSLATION_PROFILE_ID, VideoTrackingSubmission};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

#[path = "tracking_video_fixture.rs"]
mod tracking_video_fixture;
#[path = "workspace_video.rs"]
mod workspace_video;

pub struct TrackingWorkspace {
    pub fixture: workspace_video::Fixture,
    pub dataset: Uuid,
    pub head: VideoKeyframe,
    pub segment: SegmentReceipt,
}

impl TrackingWorkspace {
    pub async fn create() -> Self {
        let fixture = workspace_video::Fixture::create_with(
            "Workspace tracking.mkv",
            tracking_video_fixture::make_motion_clip,
        )
        .await;
        let dataset = Uuid::new_v4();
        Connection::open(fixture.root.join("workspace.db"))
            .unwrap()
            .execute(
                r#"INSERT INTO datasets(dataset_id,name,categories_json,created_at_ms,updated_at_ms)
             VALUES (?1,'Tracked','[{"id":3,"name":"signal"}]',100,100)"#,
                [dataset.to_string()],
            )
            .unwrap();
        let owner = &fixture.owner;
        let session = fixture.session;
        let video = fixture.video_id;
        let frames = FrameService::new(Arc::clone(owner)).unwrap();
        let (task, job) = frames
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
        let mut worker = FrameResolveRuntime::launch(
            Arc::clone(owner),
            session,
            fixture.python.clone(),
            fixture.entry.clone(),
        )
        .await
        .unwrap();
        assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
        worker.shutdown().await.unwrap();
        let frame = frames
            .snapshot(session, task.task_id)
            .unwrap()
            .frame
            .unwrap();
        let reference = VideoFrameReference::from_resolved(task.task_id, &frame).unwrap();
        let work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
        let draft = work.save(session, KeyframeDraftRequest {
            source_video_id: video, target_dataset_id: dataset, frame_ref: reference.clone(),
            expected_video_revision: 0,
            coco_document: serde_json::to_vec(&json!({
                "images":[{"id":1,"file_name":"frame.png","width":34,"height":26}],
                "categories":[{"id":3,"name":"signal"}],
                "annotations":[{"id":7,"image_id":1,"category_id":3,"bbox":[8,8,5,5],"area":25,"iscrowd":0}],
            })).unwrap(),
        }, 231).unwrap();
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
        Self {
            fixture,
            dataset,
            head,
            segment,
        }
    }

    pub fn tracking_request(&self) -> VideoTrackingSubmission {
        VideoTrackingSubmission {
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            tracker_profile_id: TRANSLATION_PROFILE_ID,
            selection: VideoSamplingRequest {
                source_video_id: self.fixture.video_id,
                segment_id: self.segment.segment_id,
                target_dataset_id: self.dataset,
                mode: VideoSamplingMode::EveryNFrames { every_n: 2 },
                purpose: VideoSamplingPurpose::Tracking {
                    supports_time_interval: true,
                },
            },
        }
    }
}
