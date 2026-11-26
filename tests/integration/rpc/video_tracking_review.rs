use std::{fs, path::Path, sync::Arc};

use dd_infra::annotation_view::AnnotationReader;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService, ReviewTrackSelection};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::video_keyframes::{
    VideoAnnotationService, VideoSampleService, VideoSampleSubmission, VideoSamplingMode,
    VideoSamplingPurpose, VideoSamplingRequest,
};
use dd_infra::video_tracking_runtime::{VideoTrackingRuntime, VideoTrackingService};
use dd_infra::workspace_backup::task::{BackupService, BackupSubmission};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/tracking_workspace_fixture.rs"]
mod tracking_workspace_fixture;
use tracking_workspace_fixture::TrackingWorkspace;

#[tokio::test]
async fn explicit_adoption_and_the_recoverable_final_track_map_survive_reopen_and_restore() {
    let setup = TrackingWorkspace::create().await;
    let fixture = &setup.fixture;
    let session = fixture.session;
    let owner = &fixture.owner;
    let tracker = VideoTrackingService::new(Arc::clone(owner)).unwrap();
    let (tracking, job) = tracker
        .submit(session, setup.tracking_request(), 250)
        .unwrap();
    let mut runner = VideoTrackingRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(runner.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    runner.shutdown().await.unwrap();
    let tracks = tracker
        .snapshot(session, tracking.task_id)
        .unwrap()
        .bundle
        .unwrap();
    let sampler = VideoSampleService::new(Arc::clone(owner)).unwrap();
    let (sample, job) = sampler
        .submit_with_tracking(
            session,
            VideoSampleSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                selection: VideoSamplingRequest {
                    source_video_id: fixture.video_id,
                    target_dataset_id: setup.dataset,
                    segment_id: setup.segment.segment_id,
                    mode: VideoSamplingMode::EveryNFrames { every_n: 1 },
                    purpose: VideoSamplingPurpose::Dataset {
                        include_keyframes: false,
                        max_frames: Some(6),
                    },
                },
            },
            tracks.reference,
            251,
        )
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
    let samples = sampler
        .snapshot(session, sample.task_id, 252)
        .unwrap()
        .unwrap()
        .receipt
        .unwrap()
        .samples;
    let asset = samples
        .iter()
        .find(|sample| sample.frame_index == 2)
        .unwrap()
        .asset_id;
    let annotation = AnnotationReader::new(Arc::clone(owner)).unwrap();
    assert!(
        annotation
            .read(session, asset)
            .unwrap()
            .formal_coco
            .is_none()
    );
    let work = VideoAnnotationService::new(Arc::clone(owner)).unwrap();
    let candidates = work
        .staging_tracking_candidates(session, setup.dataset, asset)
        .unwrap()
        .unwrap();
    let candidate = &candidates.observations[0];
    assert_eq!(candidate.bbox_xywh, [11.0, 9.0, 5.0, 5.0]);
    let mut geometry = GeometryRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    let proof = geometry
        .audit(GeometryAuditRequest {
            dataset_id: setup.dataset,
            asset_id: asset,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            max_pixels: 10_000,
        })
        .await
        .unwrap();
    assert_eq!(proof.status, "SUCCEEDED");
    geometry.shutdown().await.unwrap();

    let review = owner
        .open_review(session, setup.dataset, asset, 253)
        .unwrap();
    let draft = ReviewDraftRequest {
        review_id: review.review_id, source_task_id: proof.task_id,
        expected_review_revision: review.row_revision, expected_draft_revision: None,
        coco_document: serde_json::to_vec(&json!({
            "images":[{"id":1,"file_name":"frame-2.png","width":34,"height":26}],
            "categories":[{"id":3,"name":"signal"}],
            "annotations":[{"id":9,"image_id":1,"category_id":3,"bbox":[12,9,5,5],"area":25,"iscrowd":0}]
        })).unwrap(),
    };
    let service = ReviewDraftService::new(Arc::clone(owner)).unwrap();
    let choice = ReviewTrackSelection {
        local_annotation_id: 9,
        track_id: candidate.track_id,
    };
    let wrong_object = ReviewTrackSelection {
        track_id: Uuid::new_v4(),
        ..choice
    };
    let wrong_id = ReviewTrackSelection {
        local_annotation_id: 10,
        ..choice
    };
    for selection in [vec![wrong_object], vec![wrong_id], vec![choice, choice]] {
        assert!(
            service
                .save_with_tracks(session, draft.clone(), &selection, 254)
                .is_err()
        );
    }
    assert!(service.read(session, review.review_id).unwrap().is_none());
    let saved = service
        .save_with_tracks(session, draft, &[choice], 255)
        .unwrap();
    let recorded = service.read(session, review.review_id).unwrap().unwrap();
    assert_eq!(recorded.receipt, saved);
    assert_eq!(recorded.track_bindings.len(), 1);
    assert_eq!(recorded.track_bindings[0].local_annotation_id, 9);
    assert_eq!(recorded.track_bindings[0].track_id, candidate.track_id);
    assert_eq!(
        recorded.track_bindings[0].tracking_task_id,
        tracking.task_id
    );
    assert_eq!(
        recorded.track_bindings[0].tracking_attempt_id,
        candidates.tracking_attempt_id
    );
    assert_eq!(
        recorded.coco_document["annotations"][0]["bbox"],
        json!([12, 9, 5, 5])
    );
    assert!(
        recorded.coco_document["annotations"][0]
            .get("track_id")
            .is_none()
    );
    assert!(
        annotation
            .read(session, asset)
            .unwrap()
            .formal_coco
            .is_none()
    );
    owner.close(session).unwrap();
    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let restored = service.read(reopened, review.review_id).unwrap().unwrap();
    assert_eq!(restored.track_bindings, recorded.track_bindings);
    assert_eq!(restored.receipt, saved);
    let continuation =
        |receipt: &dd_infra::review_draft::DraftReceipt, coco_document| ReviewDraftRequest {
            review_id: review.review_id,
            source_task_id: proof.task_id,
            expected_review_revision: receipt.review_revision,
            expected_draft_revision: Some(receipt.draft_revision),
            coco_document: serde_json::to_vec(&coco_document).unwrap(),
        };
    let mut moved = restored.coco_document.clone();
    moved["annotations"][0]["bbox"][0] = json!(13);
    let edited = service
        .save(
            reopened,
            continuation(&restored.receipt, moved.clone()),
            256,
        )
        .unwrap();
    assert_eq!(
        service
            .read(reopened, review.review_id)
            .unwrap()
            .unwrap()
            .track_bindings,
        recorded.track_bindings
    );
    moved["annotations"][0]["id"] = json!(10);
    service
        .save(reopened, continuation(&edited, moved), 257)
        .unwrap();
    assert!(
        service
            .read(reopened, review.review_id)
            .unwrap()
            .unwrap()
            .track_bindings
            .is_empty()
    );
    assert!(
        annotation
            .read(reopened, asset)
            .unwrap()
            .formal_coco
            .is_none()
    );

    let latest = service.read(reopened, review.review_id).unwrap().unwrap();
    let linked = service
        .save_with_tracks(
            reopened,
            continuation(&latest.receipt, latest.coco_document),
            &[ReviewTrackSelection {
                local_annotation_id: 10,
                ..choice
            }],
            258,
        )
        .unwrap();
    let finals = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let prepared = finals
        .prepare(
            reopened,
            ReviewFinalRequest {
                review_id: review.review_id,
                expected_review_revision: linked.review_revision,
                base_image_digest: review.base_image_digest,
                base_annotation_digest: None,
                draft_digest: linked.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: false,
            },
            259,
        )
        .unwrap();
    let read_metadata = |root: &Path| {
        let database = Connection::open(root.join("workspace.db")).unwrap();
        let metadata: String = database
            .query_row(
                "SELECT metadata_json FROM assets WHERE asset_id=?1",
                [asset.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&metadata).unwrap()
    };
    assert!(read_metadata(&fixture.root)["current_track_map_ref"].is_null());
    let prepared_map = fixture.root.join(format!(
        "artifacts/images/{asset}/track-maps/{}.json",
        prepared.operation_id
    ));
    let sealed_map = fs::read(&prepared_map).unwrap();
    fs::write(&prepared_map, b"modified track map before Final").unwrap();
    assert!(finals.resume(reopened, prepared.operation_id, 260).is_err());
    assert!(
        annotation
            .read(reopened, asset)
            .unwrap()
            .formal_coco
            .is_none()
    );
    assert!(read_metadata(&fixture.root)["current_track_map_ref"].is_null());
    fs::write(&prepared_map, sealed_map).unwrap();
    let database = Connection::open(fixture.root.join("workspace.db")).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER abort_tracking_final BEFORE UPDATE OF annotation_digest ON assets
         BEGIN SELECT RAISE(ABORT,'synthetic failure publishing tracked Final'); END;",
        )
        .unwrap();
    assert!(finals.resume(reopened, prepared.operation_id, 260).is_err());
    assert!(read_metadata(&fixture.root)["current_track_map_ref"].is_null());
    database
        .execute_batch("DROP TRIGGER abort_tracking_final")
        .unwrap();
    drop(database);
    owner.close(reopened).unwrap();
    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let recovered = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        finals
            .resume(recovered, prepared.operation_id, 261)
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    let current = annotation.read(recovered, asset).unwrap();
    let metadata = read_metadata(&fixture.root);
    let reference = &metadata["current_track_map_ref"];
    assert_eq!(reference["owner"]["kind"], "ASSET");
    assert_eq!(reference["owner"]["id"], asset.to_string());
    let relative = metadata["current_track_map_entry"]["relative_path"]
        .as_str()
        .unwrap();
    let map_path = fixture.root.join(relative);
    let bytes = fs::read(&map_path).unwrap();
    let map: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(map["asset_id"], asset.to_string());
    assert_eq!(
        map["annotation_digest"],
        current.annotation_digest.as_deref().unwrap()
    );
    assert_eq!(map["segment_id"], setup.segment.segment_id.to_string());
    assert_eq!(
        map["frame"]["source_video_id"],
        fixture.video_id.to_string()
    );
    assert_eq!(map["frame"]["frame_index"], "2");
    assert_eq!(map["bindings"][0]["local_annotation_id"], 10);
    assert_eq!(
        map["bindings"][0]["track_id"],
        candidate.track_id.to_string()
    );
    let coco = current.formal_coco.unwrap();
    assert_eq!(coco["annotations"][0]["bbox"], json!([13, 9, 5, 5]));
    assert!(coco["annotations"][0].get("track_id").is_none());

    let backup_folder = fixture.root.parent().unwrap().join("Tracked Final backup");
    fs::create_dir(&backup_folder).unwrap();
    let grant = owner
        .grant_backup_directory(recovered, backup_folder)
        .unwrap();
    let backups = BackupService::new(Arc::clone(owner));
    let (backup, job) = backups
        .submit(
            recovered,
            BackupSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                destination_grant_id: grant.grant_id,
                include_reports: true,
            },
            262,
        )
        .unwrap();
    backups
        .run_admitted(recovered, job.unwrap(), || 263)
        .unwrap();
    let package = backups
        .snapshot(recovered, backup.task_id, 264)
        .unwrap()
        .unwrap()
        .receipt
        .unwrap()
        .package;
    owner.close(recovered).unwrap();
    let restored = fixture
        .root
        .parent()
        .unwrap()
        .join("Restored tracked Final");
    let restored_session = owner
        .restore_backup(package.clone(), restored.clone())
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        read_metadata(&restored)["current_track_map_ref"],
        *reference
    );
    assert_eq!(fs::read(restored.join(relative)).unwrap(), bytes);
    assert_eq!(
        annotation
            .read(restored_session, asset)
            .unwrap()
            .formal_coco
            .unwrap(),
        coco
    );
    let track_output = Connection::open(restored.join("workspace.db"))
        .unwrap()
        .query_row(
            "SELECT output_manifest_path FROM tasks WHERE task_id=?1",
            [tracking.task_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    let retired_candidate = restored
        .join(track_output)
        .with_file_name("video-tracks.json");
    let retired_bytes = fs::read(&retired_candidate).unwrap();
    fs::write(&retired_candidate, b"retired candidate awaiting cleanup").unwrap();
    let mut geometry = GeometryRuntime::launch(
        Arc::clone(owner),
        restored_session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    let checked_final = geometry
        .audit(GeometryAuditRequest {
            dataset_id: setup.dataset,
            asset_id: asset,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            max_pixels: 10_000,
        })
        .await
        .unwrap();
    assert_eq!(checked_final.status, "SUCCEEDED");
    geometry.shutdown().await.unwrap();
    let next_review = owner
        .open_review(restored_session, setup.dataset, asset, 265)
        .unwrap();
    let initial = service
        .editor_state(restored_session, next_review.review_id)
        .unwrap();
    assert!(initial.draft.is_none());
    assert_eq!(initial.initial_track_bindings[0].local_annotation_id, 10);
    assert_eq!(initial.initial_track_bindings[0].track_id, choice.track_id);
    let mut next_coco = coco.clone();
    next_coco["annotations"][0]["bbox"][0] = json!(14);
    let draft_request = |document| ReviewDraftRequest {
        review_id: next_review.review_id,
        source_task_id: checked_final.task_id,
        expected_review_revision: next_review.row_revision,
        expected_draft_revision: None,
        coco_document: serde_json::to_vec(&document).unwrap(),
    };
    let mut incorrect = next_coco.clone();
    incorrect["annotations"][0]["id"] = json!(11);
    assert!(
        service
            .save_with_tracks(
                restored_session,
                draft_request(incorrect),
                &[ReviewTrackSelection {
                    local_annotation_id: 11,
                    ..choice
                }],
                266
            )
            .is_err()
    );
    let next_draft = service
        .save_with_tracks(
            restored_session,
            draft_request(next_coco.clone()),
            &[ReviewTrackSelection {
                local_annotation_id: 10,
                ..choice
            }],
            266,
        )
        .unwrap();
    let next_final = finals
        .prepare(
            restored_session,
            ReviewFinalRequest {
                review_id: next_review.review_id,
                expected_review_revision: next_draft.review_revision,
                base_image_digest: next_review.base_image_digest,
                base_annotation_digest: next_review.base_annotation_digest,
                draft_digest: next_draft.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: false,
            },
            267,
        )
        .unwrap();
    assert_eq!(
        finals
            .resume(restored_session, next_final.operation_id, 268)
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    let updated_metadata = read_metadata(&restored);
    assert_ne!(updated_metadata["current_track_map_ref"], *reference);
    let updated_path = updated_metadata["current_track_map_entry"]["relative_path"]
        .as_str()
        .unwrap();
    let updated_map: serde_json::Value =
        serde_json::from_slice(&fs::read(restored.join(updated_path)).unwrap()).unwrap();
    let updated_view = annotation.read(restored_session, asset).unwrap();
    assert_eq!(updated_view.formal_coco.unwrap(), next_coco);
    assert_eq!(
        updated_map["annotation_digest"],
        updated_view.annotation_digest.unwrap()
    );
    assert_ne!(updated_map["annotation_digest"], map["annotation_digest"]);
    assert_eq!(updated_map["bindings"], map["bindings"]);
    assert_eq!(updated_map["frame"], map["frame"]);
    fs::write(retired_candidate, retired_bytes).unwrap();
    owner.close(restored_session).unwrap();
    let rejected = fixture
        .root
        .parent()
        .unwrap()
        .join("Rejected tracked backup");
    let package_map = package.join(relative);
    fs::write(&package_map, b"modified map in backup").unwrap();
    assert!(owner.restore_backup(package, rejected.clone()).is_err());
    assert!(!rejected.exists());
    fs::write(&package_map, bytes).unwrap();
    assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
}
