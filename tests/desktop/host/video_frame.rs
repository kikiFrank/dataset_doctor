use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::media_display::MediaDisplayService;
use dd_ui_contracts::annotation::MediaGetLocatorRequest;
use dd_ui_contracts::dataset_import::{ImportOperationKind, TaskGetRequest};
use dd_ui_contracts::video::{FrameResolution, VideoImportRequest, VideoResolveFrameRequest};
use tauri::http::Request;

use super::{video::VideoRunner, *};

#[path = "../../integration/rpc/support/video_fixture.rs"]
mod video_fixture;

#[test]
fn desktop_resolves_and_displays_a_real_frame_and_reopens_the_verified_cached_result() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let root = run.join("Desktop frame Workspace");
    fs::create_dir(&root).unwrap();
    let clip = run.join("Desktop two color frame.mp4");
    video_fixture::make_clip(&clip);
    let original = fs::read(&clip).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let workspace = owner
        .create(grant.grant_id, "Desktop frames".into(), 100)
        .unwrap();
    let uuid = |value: String| UuidText::try_from(value).unwrap();
    let context = |session: Uuid| CommandContext {
        request_id: uuid(Uuid::new_v4().to_string()),
        workspace_id: uuid(workspace.workspace_id.clone()),
        workspace_session_id: uuid(session.to_string()),
        idempotency_key: Some(uuid(Uuid::new_v4().to_string())),
    };
    let session = workspace.workspace_session_id;
    let commands = Commands {
        thread: Arc::clone(&owner),
        request_id: uuid(Uuid::new_v4().to_string()),
    };
    let runner = VideoRunner::new(
        repo.join("target/dev/worker-venv/Scripts/python.exe"),
        repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
    );
    let grant = commands.video_file(context(session), clip.clone()).unwrap();
    let imported = commands
        .start_video_import(
            Arc::clone(&runner),
            VideoImportRequest {
                context: context(session),
                file_grant_id: grant.grant_id,
                display_name: None,
                stream_index: None,
            },
        )
        .unwrap();
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let wait = |session, mut task_id: UuidText| {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let snapshot = commands
                .import_task(
                    &imports,
                    TaskGetRequest {
                        context: context(session),
                        task_id,
                    },
                )
                .unwrap();
            if !matches!(
                snapshot.status.as_str(),
                "QUEUED" | "DISPATCHING" | "RUNNING"
            ) {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
            task_id = snapshot.task_id;
        }
    };
    assert_eq!(
        wait(session, imported.operation.task_id).status,
        "SUCCEEDED"
    );
    let request = |session: Uuid, target: serde_json::Value| {
        serde_json::from_value::<VideoResolveFrameRequest>(serde_json::json!({
            "context": {
                "request_id": Uuid::new_v4().to_string(),
                "workspace_id": workspace.workspace_id,
                "workspace_session_id": session.to_string(),
                "idempotency_key": Uuid::new_v4().to_string()
            },
            "source_video_id": String::from(imported.source_video_id.clone()),
            "target": target,
            "seek_policy": "AT_OR_AFTER"
        }))
    };
    assert!(
        request(
            session,
            serde_json::json!({"timestamp_ms":"401","frame_index":"0"})
        )
        .is_err()
    );
    let initial_request = request(session, serde_json::json!({"timestamp_ms":"401"})).unwrap();
    let original_key = initial_request.context.idempotency_key.clone();
    let initial = commands
        .resolve_video_frame(Some(Arc::clone(&runner)), initial_request)
        .unwrap();
    let FrameResolution::Accepted { operation } = initial else {
        panic!("first frame request must be queued");
    };
    assert!(matches!(
        operation.operation_kind,
        ImportOperationKind::FrameResolve
    ));
    let done = wait(session, operation.task_id);
    assert_eq!(done.status, "SUCCEEDED");
    let mut conflicting = request(session, serde_json::json!({"frame_index":"0"})).unwrap();
    conflicting.context.idempotency_key = original_key;
    assert_eq!(
        commands
            .resolve_video_frame(Some(Arc::clone(&runner)), conflicting)
            .unwrap_err()
            .code,
        "CONFLICT"
    );
    let frame = done.video_frame.unwrap();
    assert_eq!(String::from(frame.frame_index), "3");
    assert_eq!(String::from(frame.pts_us.unwrap()), "600000");
    let owner_task = match &frame.image_ref.owner {
        dd_ui_contracts::ArtifactOwner::Task(task) => String::from(task.clone()),
        _ => panic!("a resolved frame must belong to its task"),
    };
    let media_request = |session: Uuid, artifact: String| {
        serde_json::from_value::<MediaGetLocatorRequest>(serde_json::json!({
            "context": {
                "request_id": Uuid::new_v4().to_string(),
                "workspace_id": workspace.workspace_id,
                "workspace_session_id": session.to_string()
            },
            "artifact_id": artifact, "owner_task_id": owner_task, "purpose":"DISPLAY"
        }))
        .unwrap()
    };
    let media = MediaDisplayService::new(Arc::clone(&owner), GRANT_LIFETIME);
    assert!(
        commands
            .media_locator(&media, media_request(session, Uuid::new_v4().to_string()))
            .is_err()
    );
    let mut ambiguous = media_request(session, String::from(frame.image_ref.artifact_id.clone()));
    ambiguous.asset_id = Some(imported.source_video_id.clone());
    assert_eq!(
        commands.media_locator(&media, ambiguous).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    let locator = commands
        .media_locator(
            &media,
            media_request(session, String::from(frame.image_ref.artifact_id.clone())),
        )
        .unwrap();
    let response = annotation::media_response(
        &media,
        Request::builder()
            .uri(locator.locator.as_str())
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(response.status(), 200);
    assert!(response.body().starts_with(b"\x89PNG\r\n\x1a\n"));
    let displayed = run.join("Displayed synthetic blue frame.png");
    fs::write(&displayed, response.body()).unwrap();
    let pixels = std::process::Command::new(repo.join("target/dev/worker-venv/Scripts/python.exe"))
        .args(["-I","-B","-c","import sys; from PIL import Image; im=Image.open(sys.argv[1]); im.load(); r,g,b=im.getpixel((20,12)); assert b>240 and max(r,g)<12"])
        .arg(displayed).status().unwrap();
    assert!(pixels.success());
    owner.close(session).unwrap();
    let response = annotation::media_response(
        &media,
        Request::builder()
            .uri(locator.locator.as_str())
            .body(Vec::new())
            .unwrap(),
    );
    assert_eq!(response.status(), 403);
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let cached = commands
        .resolve_video_frame(
            None,
            request(reopened, serde_json::json!({"frame_index":"3"})).unwrap(),
        )
        .unwrap();
    let FrameResolution::Ready { frame: cached } = cached else {
        panic!("reopened frame should use the verified cache");
    };
    assert_eq!(cached.image_ref.artifact_id, frame.image_ref.artifact_id);
    let failed = commands
        .resolve_video_frame(
            Some(VideoRunner::new(
                run.join("absent-frame-worker.exe"),
                run.join("absent-frame-worker.py"),
            )),
            request(reopened, serde_json::json!({"frame_index":"0"})).unwrap(),
        )
        .unwrap();
    let FrameResolution::Accepted { operation } = failed else {
        panic!("uncached frame must be a task");
    };
    let failed = wait(reopened, operation.task_id);
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("WORKER_START_FAILED"));
    let absent = commands
        .resolve_video_frame(
            None,
            request(reopened, serde_json::json!({"frame_index":"1"})).unwrap(),
        )
        .unwrap();
    let FrameResolution::Accepted { operation } = absent else {
        panic!("unconfigured uncached frame must return a task");
    };
    let failed = wait(reopened, operation.task_id);
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("WORKER_START_FAILED"));
    assert_eq!(fs::read(clip).unwrap(), original);
}
