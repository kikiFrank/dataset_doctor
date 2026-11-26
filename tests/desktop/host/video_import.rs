use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::dataset_import::{ImportOperationKind, TaskGetRequest};
use dd_ui_contracts::video::VideoImportRequest;

use super::{video::VideoRunner, *};

#[test]
fn desktop_video_import_returns_a_registered_asset_and_queryable_background_task() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        run.parent(),
        Some(repo.join("target/desktop-commands/runs").as_path())
    );
    let root = run.join("Desktop video Workspace");
    fs::create_dir(&root).unwrap();
    let source = run.join("Selected video.mp4");
    let original = b"synthetic file for the desktop registration and launch-failure path";
    fs::write(&source, original).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = thread.grant_selected_directory(root).unwrap();
    let workspace = thread
        .create(grant.grant_id, "Desktop video".into(), 100)
        .unwrap();
    let session = workspace.workspace_session_id;
    let id = |value: String| UuidText::try_from(value).unwrap();
    let context = || CommandContext {
        request_id: id(Uuid::new_v4().to_string()),
        workspace_id: id(workspace.workspace_id.clone()),
        workspace_session_id: id(session.to_string()),
        idempotency_key: None,
    };
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: id(Uuid::new_v4().to_string()),
    };
    let submit = |runner, path: PathBuf| {
        let selected = commands.video_file(context(), path).unwrap();
        assert_eq!(selected.allowed_operations, ["VIDEO_IMPORT"]);
        let mut admission = context();
        admission.idempotency_key = Some(id(Uuid::new_v4().to_string()));
        commands
            .start_video_import(
                runner,
                VideoImportRequest {
                    context: admission,
                    file_grant_id: selected.grant_id,
                    display_name: Some("Selected clip".into()),
                    stream_index: None,
                },
            )
            .unwrap()
    };
    let imports = DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap();
    let await_task = |task_id: UuidText| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let record = commands
                .import_task(
                    &imports,
                    TaskGetRequest {
                        context: context(),
                        task_id: task_id.clone(),
                    },
                )
                .unwrap();
            assert!(matches!(
                record.operation_kind,
                ImportOperationKind::VideoProbe
            ));
            if !matches!(record.status.as_str(), "QUEUED" | "DISPATCHING" | "RUNNING") {
                return record;
            }
            assert!(
                Instant::now() < deadline,
                "last video probe state was {}",
                record.status
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let receipt = submit(
        VideoRunner::new(run.join("absent-python.exe"), run.join("absent-worker.py")),
        source.clone(),
    );
    assert!(matches!(
        receipt.operation.operation_kind,
        ImportOperationKind::VideoProbe
    ));
    assert!(Uuid::parse_str(&String::from(receipt.source_video_id)).is_ok());
    let failed = await_task(receipt.operation.task_id);
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("WORKER_START_FAILED"));
    assert_eq!(fs::read(source).unwrap(), original);

    if let Some(ffmpeg) = std::env::var_os("DD_WORKER_VIDEO_FFMPEG") {
        let source = run.join("Video with real frames.mp4");
        make_clip(Path::new(&ffmpeg), &source);
        let bytes = fs::read(&source).unwrap();
        let selected = submit(
            VideoRunner::new(
                repo.join("target/dev/worker-venv/Scripts/python.exe"),
                repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
            ),
            source.clone(),
        );
        let finished = await_task(selected.operation.task_id);
        assert_eq!(finished.status, "SUCCEEDED");
        assert!(finished.error_code.is_none());
        let detail = commands
            .video_detail(dd_ui_contracts::video::VideoGetRequest {
                context: context(),
                source_video_id: selected.source_video_id.clone(),
            })
            .unwrap();
        assert_eq!(
            detail.source_availability,
            dd_ui_contracts::video::VideoAvailability::Online
        );
        assert_eq!(detail.duration_ms, Some(500_u64.into()));
        assert_eq!(detail.streams.len(), 1);
        assert_eq!(detail.rotation_ccw, Some(0));
        assert_eq!(detail.variable_frame_rate, None);
        let old_session = session;
        thread.close(old_session).unwrap();
        let root = run.join("Desktop video Workspace");
        let directory = thread.grant_selected_directory(root).unwrap();
        let reopened = thread.open(directory.grant_id).unwrap();
        let mut restored_context = context();
        restored_context.workspace_session_id = id(reopened.workspace_session_id.to_string());
        let detail = commands
            .video_detail(dd_ui_contracts::video::VideoGetRequest {
                context: restored_context,
                source_video_id: selected.source_video_id,
            })
            .unwrap();
        assert_eq!(detail.duration_ms, Some(500_u64.into()));
        assert_eq!(fs::read(source).unwrap(), bytes);
        println!("The desktop host registered, probed and reopened a real synthetic clip");
    }
}

fn make_clip(ffmpeg: &Path, output: &Path) {
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
            "color=red:s=48x32:r=4:d=0.5",
            "-c:v",
            "mpeg4",
            "-threads",
            "1",
        ])
        .arg(output);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    assert!(command.status().unwrap().success());
}
