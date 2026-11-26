use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use dd_infra::video_annotation_store::VideoArtifactOwner;
use dd_infra::video_keyframes::{KeyframeDraftRequest, VideoAnnotationService};
use dd_infra::workspace_backup::task::{BackupService, BackupSubmission};
use rusqlite::{Connection, params};
use serde_json::json;
use uuid::Uuid;

#[path = "support/video_annotation_fixture.rs"]
mod video_annotation_fixture;
use video_annotation_fixture::{document, prepared};

#[tokio::test]
async fn real_frame_drafts_validate_persist_and_survive_full_backup_restore() {
    let (fixture, dataset, frame) = prepared().await;
    let service = VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap();
    let initial = service.read(fixture.session, fixture.video_id).unwrap();
    assert!(initial.manifest.is_none());
    Connection::open(fixture.root.join("workspace.db")).unwrap().execute(
        "UPDATE assets SET metadata_json='  ' || json_set(metadata_json,'$.unrelated',7) WHERE asset_id=?1",
        [fixture.video_id.to_string()]).unwrap();
    let mut request = KeyframeDraftRequest {
        source_video_id: fixture.video_id,
        target_dataset_id: dataset,
        frame_ref: frame.clone(),
        coco_document: serde_json::to_vec(&document()).unwrap(),
        expected_video_revision: initial.video_revision,
    };
    assert!(service.save(Uuid::new_v4(), request.clone(), 300).is_err());
    for bad in [
        json!([47, 3, 10, 8]),
        json!([2, 3, 0, 8]),
        json!([-1, 3, 10, 8]),
    ] {
        let mut doc = document();
        doc["annotations"][0]["bbox"] = bad;
        let mut invalid = request.clone();
        invalid.coco_document = serde_json::to_vec(&doc).unwrap();
        assert!(service.save(fixture.session, invalid, 301).is_err());
    }
    for (key, value) in [("width", json!(49)), ("file_name", json!("../escape.png"))] {
        let mut doc = document();
        doc["images"][0][key] = value;
        let mut invalid = request.clone();
        invalid.coco_document = serde_json::to_vec(&doc).unwrap();
        assert!(service.save(fixture.session, invalid, 302).is_err());
    }
    let mut unknown = document();
    unknown["categories"][0]["id"] = json!(2);
    unknown["annotations"][0]["category_id"] = json!(2);
    let mut invalid = request.clone();
    invalid.coco_document = serde_json::to_vec(&unknown).unwrap();
    assert!(service.save(fixture.session, invalid, 303).is_err());
    let mut invalid = request.clone();
    invalid.frame_ref.frame_index = "2".into();
    assert!(service.save(fixture.session, invalid, 304).is_err());
    let mut invalid = request.clone();
    invalid.frame_ref.image_ref.sha256 = "0".repeat(64);
    assert!(service.save(fixture.session, invalid, 305).is_err());
    let mut invalid = request.clone();
    invalid.frame_ref.image_ref.owner = VideoArtifactOwner::Asset(fixture.video_id);
    assert!(service.save(fixture.session, invalid, 306).is_err());
    assert_eq!(
        service
            .read(fixture.session, fixture.video_id)
            .unwrap()
            .video_revision,
        initial.video_revision
    );
    let manifests = fixture
        .root
        .join(format!("artifacts/videos/{}/manifests", fixture.video_id));
    fs::create_dir_all(manifests.parent().unwrap()).unwrap();
    fs::write(&manifests, b"synthetic publication failure").unwrap();
    assert!(service.save(fixture.session, request.clone(), 307).is_err());
    assert!(
        service
            .read(fixture.session, fixture.video_id)
            .unwrap()
            .manifest
            .is_none()
    );
    fs::remove_file(&manifests).unwrap();
    let saved = service.save(fixture.session, request.clone(), 310).unwrap();
    assert_eq!(saved.video_revision, initial.video_revision + 1);
    assert_eq!(
        saved.annotation_ref.owner,
        VideoArtifactOwner::Asset(fixture.video_id)
    );
    assert_eq!(saved.draft_digest, saved.annotation_ref.sha256);
    let loaded = service
        .load_draft(
            fixture.session,
            fixture.video_id,
            saved.annotation_ref.clone(),
        )
        .unwrap();
    assert_eq!(loaded.coco_document["annotations"][0]["area"], 80.0);
    assert!(service.save(fixture.session, request.clone(), 311).is_err());
    request.expected_video_revision = saved.video_revision;
    let mut negative = document();
    negative["annotations"] = json!([]);
    request.coco_document = serde_json::to_vec(&negative).unwrap();
    let replaced = service.save(fixture.session, request, 312).unwrap();
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
    assert!(state.manifest.as_ref().unwrap().segments.is_empty());
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM assets),
        (SELECT COUNT(*) FROM dataset_assets),(SELECT COUNT(*) FROM review_sessions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 0, 0));
    assert_eq!(
        connection
            .query_row(
                "SELECT json_extract(metadata_json,'$.unrelated') FROM assets WHERE asset_id=?1",
                [fixture.video_id.to_string()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        7
    );
    drop(connection);
    let target = fixture.root.parent().unwrap().join("Keyframe backup");
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
    let receipt = backup
        .snapshot(fixture.session, queued.task_id, 322)
        .unwrap()
        .unwrap()
        .receipt
        .unwrap();
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
    let loaded = service
        .load_draft(reopened, fixture.video_id, replaced.annotation_ref.clone())
        .unwrap();
    assert!(
        loaded.coco_document["annotations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(service.read(fixture.session, fixture.video_id).is_err());
    fixture.owner.close(reopened).unwrap();
    let restored = fixture.root.parent().unwrap().join("Restored keyframes");
    let reopened = fixture
        .owner
        .restore_backup(receipt.package, restored.clone())
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        service.read(reopened, fixture.video_id).unwrap().manifest,
        state.manifest
    );
    assert_eq!(
        service
            .load_draft(reopened, fixture.video_id, replaced.annotation_ref.clone())
            .unwrap()
            .coco_document,
        loaded.coco_document
    );
    assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
    let draft = restored.join(format!(
        "artifacts/videos/{}/keyframes/{}.json",
        fixture.video_id, replaced.annotation_ref.artifact_id
    ));
    fs::write(draft, b"corrupted keyframe").unwrap();
    assert!(service.read(reopened, fixture.video_id).is_err());
}

#[tokio::test]
async fn concurrent_draft_saves_reject_stale_revisions_and_changed_source_or_categories() {
    let (fixture, dataset, frame) = prepared().await;
    let service = Arc::new(VideoAnnotationService::new(Arc::clone(&fixture.owner)).unwrap());
    let revision = service
        .read(fixture.session, fixture.video_id)
        .unwrap()
        .video_revision;
    let request = KeyframeDraftRequest {
        source_video_id: fixture.video_id,
        target_dataset_id: dataset,
        frame_ref: frame,
        coco_document: serde_json::to_vec(&document()).unwrap(),
        expected_video_revision: revision,
    };
    fs::write(&fixture.source, b"source changed").unwrap();
    assert!(service.save(fixture.session, request.clone(), 400).is_err());
    fs::write(&fixture.source, &fixture.original).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let attempts: Vec<_> = (0..2)
        .map(|_| {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            let request = request.clone();
            let session = fixture.session;
            thread::spawn(move || {
                barrier.wait();
                service.save(session, request, 401)
            })
        })
        .collect();
    let outcomes: Vec<_> = attempts.into_iter().map(|a| a.join().unwrap()).collect();
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    let state = service.read(fixture.session, fixture.video_id).unwrap();
    let mut changed = request;
    changed.expected_video_revision = state.video_revision;
    Connection::open(fixture.root.join("workspace.db")).unwrap().execute(
        "UPDATE datasets SET categories_json='[{\"id\":1,\"name\":\"renamed\"}]' WHERE dataset_id=?1",
        params![dataset.to_string()]).unwrap();
    assert!(service.save(fixture.session, changed, 402).is_err());
    assert_eq!(
        service
            .read(fixture.session, fixture.video_id)
            .unwrap()
            .video_revision,
        state.video_revision
    );
}
