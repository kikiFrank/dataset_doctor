use std::sync::Arc;

use dd_infra::annotation_view::AnnotationReader;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::staging_report::{
    OptionalStagingCheck, StagingReport, StagingReportRequest, StagingReportService,
};
use dd_infra::video_annotation_store::{VideoArtifactOwner, VideoKeyframe};
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationService, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use uuid::Uuid;

#[path = "video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

pub async fn ready() -> (
    video_annotation_fixture::Fixture,
    Uuid,
    Uuid,
    Uuid,
    StagingReport,
) {
    let (fixture, dataset, frame) = prepared().await;
    let session = fixture.session;
    let owner = &fixture.owner;
    let video = fixture.video_id;
    let VideoArtifactOwner::Task(frame_task) = frame.image_ref.owner else {
        panic!("expected an accepted frame task");
    };
    let work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
    let keyframe = work
        .save(
            session,
            KeyframeDraftRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                frame_ref: frame.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: 0,
            },
            300,
        )
        .unwrap();
    let segment = work
        .save_segment(
            session,
            SegmentRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                segment_id: None,
                start_us: 600_000,
                end_us: 600_001,
                keyframes: vec![VideoKeyframe {
                    frame_ref: frame,
                    annotation_ref: keyframe.annotation_ref,
                }],
                expected_revision: 0,
            },
            301,
        )
        .unwrap();
    let plan = work
        .resolve_sampling(
            session,
            VideoSamplingRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                segment_id: segment.segment_id,
                mode: VideoSamplingMode::EveryNFrames { every_n: 1 },
                purpose: VideoSamplingPurpose::Dataset {
                    include_keyframes: true,
                    max_frames: Some(1),
                },
            },
        )
        .unwrap();
    let asset = work
        .stage_dataset_samples(session, &plan, &[frame_task], 302)
        .unwrap()[0]
        .asset_id;
    let mut worker = GeometryRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    let request = || GeometryAuditRequest {
        dataset_id: dataset,
        asset_id: asset,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        max_pixels: 10_000,
    };
    let audited = worker.audit(request()).await.unwrap();
    assert_eq!(audited.status, "SUCCEEDED");
    let review = owner.open_review(session, dataset, asset, 303).unwrap();
    let drafts = ReviewDraftService::new(Arc::clone(owner)).unwrap();
    let mut edited = AnnotationReader::new(Arc::clone(owner))
        .unwrap()
        .read(session, asset)
        .unwrap()
        .formal_coco
        .unwrap();
    edited["annotations"][0]["bbox"] = serde_json::json!([3, 4, 10, 8]);
    let draft = drafts
        .save(
            session,
            ReviewDraftRequest {
                review_id: review.review_id,
                source_task_id: audited.task_id,
                expected_review_revision: review.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&edited).unwrap(),
            },
            304,
        )
        .unwrap();
    let finalize = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let final_intent = finalize
        .prepare(
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
            305,
        )
        .unwrap();
    assert_eq!(
        finalize
            .resume(session, final_intent.operation_id, 306)
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    let recheck = worker.audit(request()).await.unwrap();
    assert_eq!(recheck.status, "SUCCEEDED");
    worker.shutdown().await.unwrap();
    let reports = StagingReportService::new(Arc::clone(owner)).unwrap();
    let report = reports
        .build(
            session,
            StagingReportRequest {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                target_dataset_id: dataset,
                asset_id: asset,
                geometry_task_id: recheck.task_id,
                optional_checks: vec![OptionalStagingCheck::SemanticReview],
            },
            307,
        )
        .unwrap();
    (fixture, dataset, asset, review.review_id, report)
}
