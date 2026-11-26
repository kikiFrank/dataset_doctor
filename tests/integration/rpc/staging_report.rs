use std::{fs, sync::Arc};

use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::staging::{StagingQuery, StagingState};
use dd_infra::staging_report::{OptionalStagingCheck, StagingReportRequest, StagingReportService};
use dd_infra::video_annotation_store::{VideoArtifactOwner, VideoKeyframe};
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
async fn build_report_uses_actual_current_geometry_and_preserves_unexecuted_checks() {
    let (fixture, dataset, frame_ref) = prepared().await;
    let session = fixture.session;
    let video = fixture.video_id;
    let VideoArtifactOwner::Task(frame_task) = frame_ref.image_ref.owner else {
        panic!("expected a frame task");
    };
    let owner = &fixture.owner;
    let video_work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
    let revision = video_work.read(session, video).unwrap().video_revision;
    let draft = video_work
        .save(
            session,
            KeyframeDraftRequest {
                source_video_id: video,
                target_dataset_id: dataset,
                frame_ref: frame_ref.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: revision,
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
                    annotation_ref: draft.annotation_ref,
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
    let asset = video_work
        .stage_dataset_samples(session, &plan, &[frame_task], 302)
        .unwrap()[0]
        .asset_id;
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
    let service = StagingReportService::new(Arc::clone(owner)).unwrap();
    let request = StagingReportRequest {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        target_dataset_id: dataset,
        asset_id: asset,
        geometry_task_id: checked.task_id,
        optional_checks: vec![OptionalStagingCheck::SemanticReview],
    };
    assert!(
        service
            .build(
                session,
                StagingReportRequest {
                    geometry_task_id: frame_task,
                    ..request.clone()
                },
                303
            )
            .is_err()
    );
    let report = service.build(session, request.clone(), 304).unwrap();
    assert_eq!(report.data.checks.len(), 2);
    assert_eq!(report.data.checks[0].check_id, "annotation_geometry");
    assert_eq!(report.data.checks[0].execution_status, "COMPLETED");
    assert_eq!(report.data.checks[0].assessment, "PASS");
    assert_eq!(report.data.checks[1].check_id, "semantic_review");
    assert_eq!(report.data.checks[1].execution_status, "SKIPPED");
    assert_eq!(report.data.checks[1].assessment, "NOT_EVALUATED");
    let encoded = serde_json::to_value(&report.data).unwrap();
    assert_eq!(encoded["checks"][0]["check_contract_version"], "1");
    assert_eq!(encoded["checks"][0]["finding_count"], 0);
    assert_eq!(
        encoded["checks"][1]["skip_or_failure_reason"]["code"],
        "NO_ACCEPTED_EVIDENCE"
    );
    assert_eq!(encoded["execution_coverage"]["COMPLETED"], 1);
    assert_eq!(encoded["unexecuted_checks"][0], "semantic_review");
    let again = service.build(session, request.clone(), 305).unwrap();
    assert_eq!(again.reference, report.reference);
    assert!(
        service
            .build(
                session,
                StagingReportRequest {
                    optional_checks: vec![],
                    ..request.clone()
                },
                306
            )
            .is_err()
    );
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let current=connection.query_row(
        "SELECT t.operation_kind,t.status,s.check_task_id,t.output_manifest_path,a.annotation_path
         FROM staging_items s JOIN tasks t ON t.task_id=s.check_task_id JOIN assets a USING(asset_id)
         WHERE s.asset_id=?1",[asset.to_string()],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,
            row.get::<_,String>(3)?,row.get::<_,String>(4)?))).unwrap();
    assert_eq!(
        (current.0.as_str(), current.1.as_str(), current.2.as_str()),
        (
            "BUILD_REPORT",
            "SUCCEEDED",
            report.task_id.to_string().as_str()
        )
    );
    let stage = owner
        .list_staging(
            session,
            StagingQuery {
                target_dataset_id: Some(dataset),
                after: None,
                status: None,
                page_size: 10,
            },
        )
        .unwrap();
    assert_eq!(stage.items[0].state, StagingState::NeedsReview);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_report_binding BEFORE UPDATE OF check_task_id ON staging_items
         BEGIN SELECT RAISE(ABORT, 'injected report binding failure'); END;",
        )
        .unwrap();
    assert!(
        service
            .build(
                session,
                StagingReportRequest {
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                    ..request.clone()
                },
                307
            )
            .is_err()
    );
    let (report_count, bound_task) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM tasks WHERE operation_kind='BUILD_REPORT'),check_task_id
         FROM staging_items WHERE asset_id=?1",
            [asset.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
    assert_eq!(report_count, 1);
    assert_eq!(bound_task, report.task_id.to_string());
    connection
        .execute_batch("DROP TRIGGER reject_report_binding;")
        .unwrap();
    let annotation = fixture.root.join(current.4);
    let original = fs::read(&annotation).unwrap();
    fs::write(&annotation, b"changed after the geometry task").unwrap();
    assert!(
        service
            .build(
                session,
                StagingReportRequest {
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                    ..request.clone()
                },
                307
            )
            .is_err()
    );
    fs::write(annotation, original).unwrap();
    assert_eq!(std::fs::read(&fixture.source).unwrap(), fixture.original);
    drop(connection);
    owner.close(session).unwrap();
    let folder = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(folder.grant_id).unwrap().workspace_session_id;
    assert!(service.get(session, report.task_id).is_err());
    assert_eq!(
        service.get(reopened, report.task_id).unwrap().reference,
        report.reference
    );
    fs::write(fixture.root.join(current.3), b"tampered report").unwrap();
    assert!(service.get(reopened, report.task_id).is_err());
}
