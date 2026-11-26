use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dd_infra::video_registration::{VideoImport, VideoImportError, VideoRegistry};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const SOURCE: &[u8] = b"synthetic video locator fixture, probe intentionally not yet run";

fn fixture(root: &Path) -> (Arc<WorkspaceThread>, Uuid) {
    fs::create_dir(root).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(5)).unwrap());
    let grant = owner.grant_selected_directory(root.to_owned()).unwrap();
    let workspace = owner
        .create(grant.grant_id, "Video registry".into(), 100)
        .unwrap();
    (owner, workspace.workspace_session_id)
}

#[test]
fn one_admission_registers_an_external_video_and_a_queryable_probe_without_copying_source() {
    let run = tempfile::tempdir().unwrap();
    let root = run.path().join("workspace");
    let media = run.path().join("video.mp4");
    fs::write(&media, SOURCE).unwrap();
    let (owner, session) = fixture(&root);
    let registry = VideoRegistry::new(Arc::clone(&owner)).unwrap();
    let grant = owner.grant_video_file(session, media.clone()).unwrap();
    let mut request = VideoImport {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        file_grant_id: grant.grant_id,
        display_name: Some("Synthetic clip".into()),
        stream_index: Some(0),
    };
    let first = registry.admit(session, request.clone(), 200).unwrap();
    assert_eq!(
        first,
        registry.admit(session, request.clone(), 201).unwrap()
    );
    assert_eq!(
        owner.task_outcome(session, first.task_id).unwrap().status,
        "QUEUED"
    );
    assert_eq!(fs::read(&media).unwrap(), SOURCE);
    let replay = request.clone();
    request.stream_index = Some(1);
    assert!(matches!(
        registry.admit(session, request, 202),
        Err(VideoImportError::IdempotencyConflict)
    ));
    let reopened = owner.grant_selected_directory(root.clone()).unwrap();
    owner.close(session).unwrap();
    let session = owner.open(reopened.grant_id).unwrap().workspace_session_id;
    assert_eq!(first, registry.admit(session, replay, 203).unwrap());
    owner.close(session).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let values: (String, String, String, String) = db
        .query_row(
            "SELECT kind, location_kind, path, media_digest FROM assets WHERE asset_id = ?1",
            [first.video_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(values.0, "VIDEO");
    assert_eq!(values.1, "EXTERNAL");
    assert_eq!(Path::new(&values.2), media.canonicalize().unwrap());
    assert_eq!(values.3, format!("{:x}", Sha256::digest(SOURCE)));
    let (capability, path): (String, String) = db
        .query_row(
            "SELECT capability_id, input_manifest_path FROM tasks WHERE task_id = ?1",
            [first.task_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(capability, "video_probe.v1");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(path)).unwrap()).unwrap();
    assert_eq!(
        manifest["assets"][0]["asset_id"],
        first.video_id.to_string()
    );
    assert_eq!(manifest["assets"][0]["media_digest"], values.3);
    assert_eq!(manifest["resolved_config"]["stream_index"], 0);
    assert!(!root.join("video.mp4").exists());
}

#[test]
fn failed_probe_task_insert_rolls_back_the_registration_and_a_fresh_grant_can_retry() {
    let run = tempfile::tempdir().unwrap();
    let root = run.path().join("workspace");
    let media = run.path().join("video.mp4");
    fs::write(&media, SOURCE).unwrap();
    let (owner, session) = fixture(&root);
    let registry = VideoRegistry::new(Arc::clone(&owner)).unwrap();
    let injector = Connection::open(root.join("workspace.db")).unwrap();
    injector.execute_batch(
        "CREATE TRIGGER reject_video_probe BEFORE INSERT ON tasks WHEN NEW.operation_kind='VIDEO_PROBE'
         BEGIN SELECT RAISE(ABORT, 'injected probe insert failure'); END;",
    ).unwrap();
    let mut request = VideoImport {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        file_grant_id: owner
            .grant_video_file(session, media.clone())
            .unwrap()
            .grant_id,
        display_name: None,
        stream_index: None,
    };
    assert!(registry.admit(session, request.clone(), 200).is_err());
    assert_eq!(
        injector
            .query_row("SELECT count(*) FROM assets", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert!(!root.join("artifacts/task-inputs").exists());
    assert_eq!(
        injector
            .query_row("SELECT count(*) FROM tasks", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        0
    );
    injector
        .execute("DROP TRIGGER reject_video_probe", [])
        .unwrap();
    request.file_grant_id = owner
        .grant_video_file(session, media.clone())
        .unwrap()
        .grant_id;
    let queued = registry.admit(session, request, 201).unwrap();
    assert_eq!(
        owner.task_outcome(session, queued.task_id).unwrap().status,
        "QUEUED"
    );
    assert_eq!(fs::read(media).unwrap(), SOURCE);
}
