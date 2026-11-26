use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::video_registration::{VideoImport, VideoRegistry};

use super::{video::VideoRunner, *};

#[test]
fn reopened_desktop_rebinds_and_resumes_the_queued_video_task() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/desktop-commands/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let root = run.join("Video recovery Workspace");
    fs::create_dir(&root).unwrap();
    let source = run.join("recovery.mp4");
    fs::write(&source, b"synthetic video locator for the recovery fixture").unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let selected = thread.grant_selected_directory(root.clone()).unwrap();
    let workspace = thread
        .create(selected.grant_id, "Recovery video".into(), 100)
        .unwrap();
    let session = workspace.workspace_session_id;
    let selected = thread.grant_video_file(session, source.clone()).unwrap();
    let registry = VideoRegistry::new(Arc::clone(&thread)).unwrap();
    let receipt = registry
        .admit(
            session,
            VideoImport {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                file_grant_id: selected.grant_id,
                display_name: None,
                stream_index: None,
            },
            200,
        )
        .unwrap();
    let selected = thread.grant_video_file(session, source).unwrap();
    let damaged = registry
        .admit(
            session,
            VideoImport {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                file_grant_id: selected.grant_id,
                display_name: None,
                stream_index: Some(2),
            },
            201,
        )
        .unwrap();
    let manifest = fs::read_dir(root.join("artifacts/task-inputs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let body: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            body["resolved_config"]["stream_index"] == 2
        })
        .unwrap();
    fs::write(manifest, b"tampered synthetic input manifest").unwrap();
    assert_eq!(
        thread
            .task_outcome(session, receipt.task_id)
            .unwrap()
            .status,
        "QUEUED"
    );
    thread.close(session).unwrap();
    let selected = thread.grant_selected_directory(root).unwrap();
    let reopened = thread.open(selected.grant_id).unwrap().workspace_session_id;
    assert_ne!(reopened, session);
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: UuidText::try_from(Uuid::new_v4().to_string()).unwrap(),
    };
    let runner = VideoRunner::new(
        run.join("absent-recovery-python.exe"),
        run.join("absent-recovery-worker.py"),
    );
    commands
        .resume_pending_video_probes(Arc::clone(&runner), reopened)
        .unwrap();
    commands
        .resume_pending_video_probes(runner, reopened)
        .unwrap();
    let invalid = thread.task_outcome(reopened, damaged.task_id).unwrap();
    assert_eq!(invalid.status, "FAILED");
    assert_eq!(
        invalid.error_code.as_deref(),
        Some("INPUT_MANIFEST_INVALID")
    );
    assert!(invalid.output_manifest_path.is_none());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let outcome = thread.task_outcome(reopened, receipt.task_id).unwrap();
        if outcome.status == "FAILED" {
            assert_eq!(outcome.error_code.as_deref(), Some("WORKER_START_FAILED"));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "last recovery state was {}",
            outcome.status
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
