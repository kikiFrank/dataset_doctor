use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use dd_infra::video_annotation_store::{VideoArtifactOwner, VideoKeyframe};
use dd_infra::video_keyframes::{KeyframeDraftRequest, SegmentRequest, VideoAnnotationService};
use dd_infra::workspace_backup::task::{BackupService, BackupSubmission};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[path = "support/video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

#[tokio::test]
async fn half_open_segments_accept_only_saved_keyframes_in_the_same_source_and_category_context() {
    let (fixture, dataset, frame) = prepared().await;
    let service = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let initial = service.read(fixture.session, fixture.video_id).unwrap();
    let draft = service
        .save(
            fixture.session,
            KeyframeDraftRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                frame_ref: frame.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: initial.video_revision,
            },
            300,
        )
        .unwrap();
    let keyframe = VideoKeyframe {
        frame_ref: frame,
        annotation_ref: draft.annotation_ref,
    };
    let request = SegmentRequest {
        source_video_id: fixture.video_id,
        target_dataset_id: dataset,
        segment_id: None,
        start_us: 600_000,
        end_us: 600_001,
        keyframes: vec![keyframe.clone()],
        expected_revision: 0,
    };
    assert!(
        service
            .save_segment(Uuid::new_v4(), request.clone(), 301)
            .is_err()
    );
    for (start, end) in [
        (600_000, 600_000),
        (600_001, 600_000),
        (0, 600_000),
        (-1, 600_001),
    ] {
        assert!(
            service
                .save_segment(
                    fixture.session,
                    SegmentRequest {
                        start_us: start,
                        end_us: end,
                        ..request.clone()
                    },
                    302
                )
                .is_err()
        );
    }
    let mut invalid = request.clone();
    invalid.keyframes[0].annotation_ref.artifact_id = Uuid::new_v4();
    assert!(service.save_segment(fixture.session, invalid, 303).is_err());
    let mut invalid = request.clone();
    invalid.keyframes.push(keyframe.clone());
    assert!(service.save_segment(fixture.session, invalid, 304).is_err());
    let mut invalid = request.clone();
    invalid.keyframes[0].frame_ref.pts_us = Some("599999".into());
    assert!(service.save_segment(fixture.session, invalid, 305).is_err());
    let other = Uuid::new_v4();
    Connection::open(fixture.root.join("workspace.db"))
        .unwrap()
        .execute(
            "INSERT INTO datasets(dataset_id,name,categories_json,created_at_ms,updated_at_ms)
         SELECT ?1,'Other',categories_json,100,100 FROM datasets WHERE dataset_id=?2",
            [other.to_string(), dataset.to_string()],
        )
        .unwrap();
    assert!(
        service
            .save_segment(
                fixture.session,
                SegmentRequest {
                    target_dataset_id: other,
                    ..request.clone()
                },
                306
            )
            .is_err()
    );
    assert_eq!(
        service
            .read(fixture.session, fixture.video_id)
            .unwrap()
            .video_revision,
        draft.video_revision
    );
    fs::write(&fixture.source, b"changed video").unwrap();
    assert!(
        service
            .save_segment(fixture.session, request.clone(), 307)
            .is_err()
    );
    fs::write(&fixture.source, &fixture.original).unwrap();
    let saved = service
        .save_segment(fixture.session, request.clone(), 310)
        .unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(
        saved.video_annotation_manifest_ref.owner,
        VideoArtifactOwner::Asset(fixture.video_id)
    );
    let state = service.read(fixture.session, fixture.video_id).unwrap();
    let manifest = state.manifest.as_ref().unwrap();
    assert_eq!(manifest.segments.len(), 1);
    assert_eq!(manifest.segments[0].start_us, "600000");
    assert_eq!(manifest.segments[0].end_us, "600001");
    assert_eq!(manifest.segments[0].keyframes, vec![keyframe.clone()]);
    assert!(manifest.unassigned_keyframe_drafts.is_empty());
    assert!(
        service
            .load_draft(
                fixture.session,
                fixture.video_id,
                keyframe.annotation_ref.clone()
            )
            .is_ok()
    );
    let update = SegmentRequest {
        segment_id: Some(saved.segment_id),
        start_us: 0,
        end_us: 800_000,
        expected_revision: 1,
        ..request
    };
    let replaced = service
        .save_segment(fixture.session, update.clone(), 311)
        .unwrap();
    assert_eq!(replaced.revision, 2);
    assert!(
        service
            .save_segment(fixture.session, update.clone(), 312)
            .is_err()
    );
    assert!(
        service
            .save_segment(
                fixture.session,
                SegmentRequest {
                    segment_id: Some(Uuid::new_v4()),
                    ..update.clone()
                },
                313
            )
            .is_err()
    );
    assert!(
        service
            .save_segment(
                fixture.session,
                SegmentRequest {
                    target_dataset_id: other,
                    expected_revision: 2,
                    ..update.clone()
                },
                314
            )
            .is_err()
    );
    // Detaching a manually saved keyframe preserves it as unassigned work.
    let detached = service
        .save_segment(
            fixture.session,
            SegmentRequest {
                keyframes: vec![],
                expected_revision: 2,
                ..update
            },
            315,
        )
        .unwrap();
    assert_eq!(detached.revision, 3);
    let state = service.read(fixture.session, fixture.video_id).unwrap();
    assert_eq!(
        state
            .manifest
            .as_ref()
            .unwrap()
            .unassigned_keyframe_drafts
            .len(),
        1
    );
    assert!(
        state.manifest.as_ref().unwrap().segments[0]
            .keyframes
            .is_empty()
    );
    let target = fixture.root.parent().unwrap().join("Segment package");
    fs::create_dir(&target).unwrap();
    let grant = fixture
        .owner
        .grant_backup_directory(fixture.session, target)
        .unwrap();
    let backup = BackupService::new(Arc::clone(&fixture.owner));
    let (queued, job) = backup
        .submit(
            fixture.session,
            BackupSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                destination_grant_id: grant.grant_id,
                include_reports: true,
            },
            320,
        )
        .unwrap();
    backup
        .run_admitted(fixture.session, job.unwrap(), || 321)
        .unwrap();
    let package = backup
        .snapshot(fixture.session, queued.task_id, 322)
        .unwrap()
        .unwrap()
        .receipt
        .unwrap()
        .package;
    fixture.owner.close(fixture.session).unwrap();
    let folder = fixture
        .owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = fixture
        .owner
        .open(folder.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        service.read(reopened, fixture.video_id).unwrap().manifest,
        state.manifest
    );
    fixture.owner.close(reopened).unwrap();
    let restored = fixture
        .owner
        .restore_backup(
            package,
            fixture.root.parent().unwrap().join("Restored segments"),
        )
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        service.read(restored, fixture.video_id).unwrap().manifest,
        state.manifest
    );
    assert_eq!(fs::read(fixture.source).unwrap(), fixture.original);
}

#[tokio::test]
async fn segment_updates_compare_segment_revisions_and_reject_changed_target_categories() {
    let (fixture, dataset, frame) = prepared().await;
    let service = Arc::new(VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap());
    let revision = service
        .read(fixture.session, fixture.video_id)
        .unwrap()
        .video_revision;
    let draft = service
        .save(
            fixture.session,
            KeyframeDraftRequest {
                source_video_id: fixture.video_id,
                target_dataset_id: dataset,
                frame_ref: frame.clone(),
                coco_document: serde_json::to_vec(&document()).unwrap(),
                expected_video_revision: revision,
            },
            300,
        )
        .unwrap();
    let request = SegmentRequest {
        source_video_id: fixture.video_id,
        target_dataset_id: dataset,
        segment_id: None,
        start_us: 0,
        end_us: 800_000,
        keyframes: vec![VideoKeyframe {
            frame_ref: frame,
            annotation_ref: draft.annotation_ref,
        }],
        expected_revision: 0,
    };
    let receipt = service
        .save_segment(fixture.session, request.clone(), 301)
        .unwrap();
    let request = SegmentRequest {
        segment_id: Some(receipt.segment_id),
        expected_revision: 1,
        ..request
    };
    let ready = Arc::new(Barrier::new(2));
    let attempts: Vec<_> = (0..2)
        .map(|_| {
            let service = Arc::clone(&service);
            let ready = Arc::clone(&ready);
            let request = request.clone();
            let session = fixture.session;
            thread::spawn(move || {
                ready.wait();
                service.save_segment(session, request, 302)
            })
        })
        .collect();
    let outcomes: Vec<_> = attempts
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    let state = service.read(fixture.session, fixture.video_id).unwrap();
    Connection::open(fixture.root.join("workspace.db"))
        .unwrap()
        .execute(
            "UPDATE datasets SET categories_json='[]' WHERE dataset_id=?1",
            [dataset.to_string()],
        )
        .unwrap();
    assert!(
        service
            .save_segment(
                fixture.session,
                SegmentRequest {
                    expected_revision: 2,
                    ..request
                },
                303
            )
            .is_err()
    );
    assert_eq!(
        service
            .read(fixture.session, fixture.video_id)
            .unwrap()
            .manifest,
        state.manifest
    );
    // Even a matching file digest cannot make an invalid stored interval authoritative.
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let encoded: String = connection
        .query_row(
            "SELECT metadata_json FROM assets WHERE asset_id=?1",
            [fixture.video_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut metadata: Value = serde_json::from_str(&encoded).unwrap();
    let path = fixture.root.join(
        metadata["video_annotation_manifest_entry"]["relative_path"]
            .as_str()
            .unwrap(),
    );
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["segments"][0]["end_us"] = json!("0");
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(&path, &bytes).unwrap();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    metadata["video_annotation_manifest_ref"]["sha256"] = json!(digest);
    metadata["video_annotation_manifest_ref"]["size_bytes"] = json!(bytes.len().to_string());
    metadata["video_annotation_manifest_entry"]["reference"] =
        metadata["video_annotation_manifest_ref"].clone();
    connection
        .execute(
            "UPDATE assets SET metadata_json=?1 WHERE asset_id=?2",
            [metadata.to_string(), fixture.video_id.to_string()],
        )
        .unwrap();
    assert!(service.read(fixture.session, fixture.video_id).is_err());
}
