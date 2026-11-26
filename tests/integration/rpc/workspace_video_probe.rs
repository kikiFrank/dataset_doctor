use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use dd_infra::video_registration::{VideoImport, VideoRegistry};
use dd_infra::video_runtime::{
    VideoProbeRuntime, read_registered_video_index, read_registered_video_observation,
};
use dd_infra::workspace_thread::WorkspaceThread;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[tokio::test]
async fn a_registered_probe_survives_reopen_and_persists_a_reusable_video_index() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let gate = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/contract-check/worker-rpc")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(gate.as_path()));
    let source = run.join("workspace-probe.mp4");
    make_video(&source);
    let original = fs::read(&source).unwrap();
    let sha: [u8; 32] = Sha256::digest(&original).into();
    let root = run.join("Registered Video Workspace");
    fs::create_dir(&root).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(20)).unwrap());
    let initial = owner.grant_selected_directory(root.clone()).unwrap();
    let workspace = owner
        .create(initial.grant_id, "Registered video".into(), 100)
        .unwrap();
    let registry = VideoRegistry::new(Arc::clone(&owner)).unwrap();
    let media = owner
        .grant_video_file(workspace.workspace_session_id, source.clone())
        .unwrap();
    let registered = registry
        .admit(
            workspace.workspace_session_id,
            VideoImport {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                file_grant_id: media.grant_id,
                display_name: None,
                stream_index: Some(0),
            },
            200,
        )
        .unwrap();
    let pending = dd_infra::video_query::registered_video_detail(
        &owner,
        workspace.workspace_session_id,
        registered.video_id,
    )
    .unwrap();
    assert_eq!(pending.probe_status.as_deref(), Some("QUEUED"));
    assert!(pending.metadata.is_none());
    assert_eq!(
        pending.source_availability,
        dd_infra::video_query::VideoSourceAvailability::Online
    );
    reopen(&owner, &root, workspace.workspace_session_id);
    let session = owner.current().unwrap().unwrap().workspace_session_id;

    let mut worker = launch(&owner, session).await;
    let result = worker.run(registered).await.unwrap();
    assert_eq!(result.status, "SUCCEEDED", "{result:?}");
    assert!(result.output_manifest_path.is_some());
    assert_eq!(worker.run(registered).await.unwrap(), result);
    worker.shutdown().await.unwrap();

    reopen(&owner, &root, session);
    let session = owner.current().unwrap().unwrap().workspace_session_id;
    let stored = read_registered_video_observation(&owner, session, registered).unwrap();
    let metadata = stored.metadata.unwrap();
    assert_eq!(metadata.duration_ms, Some(1000));
    assert_eq!(metadata.variable_frame_rate, Some(false));
    assert_eq!(metadata.streams[0].stream_index, 0);
    let connection = rusqlite::Connection::open(root.join("workspace.db")).unwrap();
    connection
        .execute(
            "UPDATE assets SET availability='OFFLINE' WHERE asset_id=?1",
            [registered.video_id.to_string()],
        )
        .unwrap();
    let declared_offline =
        dd_infra::video_query::registered_video_detail(&owner, session, registered.video_id)
            .unwrap();
    assert_eq!(
        declared_offline.source_availability,
        dd_infra::video_query::VideoSourceAvailability::Offline
    );
    assert_eq!(declared_offline.metadata.unwrap().duration_ms, Some(1000));
    connection
        .execute(
            "UPDATE assets SET availability='ONLINE',
        metadata_json=json_remove(metadata_json,'$.video_probe_task_id') WHERE asset_id=?1",
            [registered.video_id.to_string()],
        )
        .unwrap();
    let legacy =
        dd_infra::video_query::registered_video_detail(&owner, session, registered.video_id)
            .unwrap();
    assert_eq!(legacy.metadata.unwrap().duration_ms, Some(1000));
    connection
        .execute(
            "UPDATE assets SET metadata_json=json_set(metadata_json,
        '$.video_probe_task_id',?2) WHERE asset_id=?1",
            [registered.video_id.to_string(), Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert!(
        dd_infra::video_query::registered_video_detail(&owner, session, registered.video_id)
            .is_err()
    );
    connection
        .execute(
            "UPDATE assets SET metadata_json=json_set(metadata_json,
        '$.video_probe_task_id',?2) WHERE asset_id=?1",
            [
                registered.video_id.to_string(),
                registered.task_id.to_string(),
            ],
        )
        .unwrap();
    drop(connection);
    let frame = stored.index.resolve_index(sha, 1).unwrap();
    assert_eq!(frame.source_video_id, registered.video_id);
    assert_eq!(frame.pts_us, Some(250_000));
    let detail =
        dd_infra::video_query::registered_video_detail(&owner, session, registered.video_id)
            .unwrap();
    assert_eq!(
        detail.source_availability,
        dd_infra::video_query::VideoSourceAvailability::Online
    );
    assert_eq!(detail.probe_status.as_deref(), Some("SUCCEEDED"));
    assert_eq!(detail.metadata.unwrap().duration_ms, Some(1000));
    assert_eq!(detail.rotation_ccw, Some(0));
    assert!(
        dd_infra::video_query::registered_video_detail(&owner, session, Uuid::new_v4()).is_err()
    );
    assert!(
        dd_infra::video_query::registered_video_detail(
            &owner,
            workspace.workspace_session_id,
            registered.video_id
        )
        .is_err()
    );
    let moved = run.join("temporarily-unavailable-video.mp4");
    fs::rename(&source, &moved).unwrap();
    let offline =
        dd_infra::video_query::registered_video_detail(&owner, session, registered.video_id)
            .unwrap();
    assert_eq!(
        offline.source_availability,
        dd_infra::video_query::VideoSourceAvailability::Offline
    );
    assert_eq!(offline.metadata.unwrap().duration_ms, Some(1000));
    fs::rename(moved, &source).unwrap();
    fs::write(&source, b"temporary changed video bytes").unwrap();
    let changed =
        dd_infra::video_query::registered_video_detail(&owner, session, registered.video_id)
            .unwrap();
    assert_eq!(
        changed.source_availability,
        dd_infra::video_query::VideoSourceAvailability::Changed
    );
    fs::write(&source, &original).unwrap();

    assert_eq!(fs::read(&source).unwrap(), original);
    let grant = owner.grant_video_file(session, source.clone()).unwrap();
    let next = registry
        .admit(
            session,
            VideoImport {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                file_grant_id: grant.grant_id,
                display_name: None,
                stream_index: None,
            },
            250,
        )
        .unwrap();
    assert_eq!(next.video_id, registered.video_id);
    fs::write(&source, b"synthetic source changed").unwrap();
    let mut worker = launch(&owner, session).await;
    let rejected = worker.run(next).await.unwrap();
    assert_eq!(rejected.status, "FAILED");
    assert_eq!(rejected.error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(rejected.output_manifest_path.is_none());
    assert!(read_registered_video_index(&owner, session, registered).is_err());
    worker.shutdown().await.unwrap();
}

async fn launch(owner: &Arc<WorkspaceThread>, session: Uuid) -> VideoProbeRuntime {
    VideoProbeRuntime::launch(
        Arc::clone(owner),
        session,
        PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap()),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../services/python-worker/src/dd_worker/entrypoint.py")
            .canonicalize()
            .unwrap(),
    )
    .await
    .unwrap()
}

fn reopen(owner: &WorkspaceThread, root: &Path, old_session: Uuid) {
    owner.close(old_session).unwrap();
    let next = owner.grant_selected_directory(root.to_owned()).unwrap();
    assert_ne!(
        owner.open(next.grant_id).unwrap().workspace_session_id,
        old_session
    );
}

fn make_video(output: &Path) {
    let ffmpeg = PathBuf::from(std::env::var_os("DD_WORKER_VIDEO_FFMPEG").unwrap());
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=48x32:r=4:d=1",
            "-c:v",
            "mpeg4",
            "-bf",
            "1",
            "-threads",
            "1",
            "-output_ts_offset",
            "2",
        ])
        .arg(output);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    assert!(command.status().unwrap().success());
}
