use std::sync::Arc;
use std::time::{Duration, Instant};

use dd_infra::annotation_view::AnnotationReader;
use dd_infra::dataset_query::AssetAnnotationState;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::media_display::MediaDisplayService;
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::staging::{StagingQuery, StagingState};
use dd_infra::video_annotation_store::{VideoArtifactOwner, VideoKeyframe};
use dd_infra::video_keyframes::{
    KeyframeDraftRequest, SegmentRequest, VideoAnnotationService, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

#[path = "support/video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

#[tokio::test]
async fn a_staged_video_frame_passes_real_check_and_explicit_final_without_membership() {
    let (fixture, dataset, frame_ref) = prepared().await;
    let session = fixture.session;
    let video = fixture.video_id;
    let VideoArtifactOwner::Task(frame_task) = frame_ref.image_ref.owner else {
        panic!("expected an accepted frame task");
    };
    let owner = &fixture.owner;
    let video_work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
    let initial = video_work.read(session, video).unwrap().video_revision;
    let saved = video_work
        .save(
            session,
            KeyframeDraftRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                frame_ref: frame_ref.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: initial,
            },
            300,
        )
        .unwrap();
    let segment = video_work
        .save_segment(
            session,
            SegmentRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                segment_id: None,
                start_us: 600_000,
                end_us: 600_001,
                keyframes: vec![VideoKeyframe {
                    frame_ref,
                    annotation_ref: saved.annotation_ref,
                }],
                expected_revision: 0,
            },
            301,
        )
        .unwrap();
    let plan = video_work
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
    let stage = video_work
        .stage_dataset_samples(session, &plan, &[frame_task], 302)
        .unwrap();
    let asset = stage[0].asset_id;
    let reader = AnnotationReader::new(Arc::clone(owner)).unwrap();
    let original = reader.read(session, asset).unwrap();
    assert_eq!(original.annotation_state, AssetAnnotationState::Unreviewed);
    let media = MediaDisplayService::new(Arc::clone(owner), Duration::from_secs(30));
    let now = Instant::now();
    let locator = media.issue(session, asset, now).unwrap();
    assert_eq!(
        media.resolve(locator.grant_id, now).unwrap().media_type,
        "image/png"
    );
    assert!(
        owner
            .open_review(session, Uuid::new_v4(), asset, 303)
            .is_err()
    );
    let mut worker =
        GeometryRuntime::launch(Arc::clone(owner), session, fixture.python, fixture.entry)
            .await
            .unwrap();
    let checked = worker
        .audit(GeometryAuditRequest {
            dataset_id: dataset,
            asset_id: asset,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            max_pixels: 10_000,
        })
        .await
        .unwrap();
    assert_eq!(checked.status, "SUCCEEDED");
    worker.shutdown().await.unwrap();
    let review = owner.open_review(session, dataset, asset, 304).unwrap();
    let mut updated = original.formal_coco.unwrap();
    updated["annotations"][0]["bbox"] = json!([5, 6, 8, 7]);
    updated["annotations"][0]["area"] = json!(56);
    let drafts = ReviewDraftService::new(Arc::clone(owner)).unwrap();
    let saved = drafts
        .save(
            session,
            ReviewDraftRequest {
                review_id: review.review_id,
                source_task_id: checked.task_id,
                expected_review_revision: review.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&updated).unwrap(),
            },
            305,
        )
        .unwrap();
    assert_ne!(
        reader.read(session, asset).unwrap().formal_coco.unwrap()["annotations"][0]["bbox"],
        json!([5, 6, 8, 7])
    );
    let finals = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let admitted = finals
        .prepare(
            session,
            ReviewFinalRequest {
                review_id: review.review_id,
                expected_review_revision: saved.review_revision,
                base_image_digest: review.base_image_digest,
                base_annotation_digest: review.base_annotation_digest,
                draft_digest: saved.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: false,
            },
            306,
        )
        .unwrap();
    assert_eq!(
        finals
            .resume(session, admitted.operation_id, 307)
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    let query = StagingQuery {
        target_dataset_id: Some(dataset),
        after: None,
        status: None,
        page_size: 10,
    };
    assert_eq!(
        owner.list_staging(session, query).unwrap().items[0].state,
        StagingState::NeedsCheck
    );
    let final_view = reader.read(session, asset).unwrap();
    assert_eq!(
        final_view.annotation_state,
        AssetAnnotationState::ConfirmedPositive
    );
    assert_eq!(
        final_view.formal_coco.unwrap()["annotations"][0]["bbox"],
        json!([5, 6, 8, 7])
    );
    assert!(
        owner
            .list_assets(session, dataset, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(std::fs::read(&fixture.source).unwrap(), fixture.original);
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT review_id FROM staging_items WHERE asset_id=?1",
                [asset.to_string()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        review.review_id.to_string()
    );
    drop(connection);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(fixture.root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        owner.list_staging(reopened, query).unwrap().items[0].state,
        StagingState::NeedsCheck
    );
    assert!(media.resolve(locator.grant_id, now).is_err());
}
