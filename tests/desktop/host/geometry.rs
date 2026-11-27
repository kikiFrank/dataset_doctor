use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::dataset_import::{GeometrySubmitRequest, TaskGetRequest};
use dd_ui_contracts::task::TaskCancelRequest;

use super::{geometry::GeometryRunner, *};

#[test]
fn desktop_geometry_worker_publishes_findings_and_startup_failure_is_terminal() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let allowed = repo.join("target/desktop-commands/runs");
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let root = run.join("Desktop geometry Workspace");
    let input = run.join("Desktop geometry inputs");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&input).unwrap();
    fs::write(
        input.join("image.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    fs::write(input.join("image.json"), br#"{"images":[{"id":1,"file_name":"image.png","width":2,"height":1}],"annotations":[{"id":7,"image_id":1,"category_id":2,"bbox":[0,0,3,1],"area":3,"iscrowd":0}],"categories":[{"id":2,"name":"helmet"}]}"#).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = thread.grant_selected_directory(root).unwrap();
    let workspace = thread.create(grant.grant_id, "Geometry".into(), 1).unwrap();
    let session = workspace.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap();
    let grant = thread.grant_import_directory(session, input).unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Images".into())
        .unwrap();
    let receipt = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap();
    let dataset_id = Uuid::parse_str(&receipt.dataset_id).unwrap();
    let asset_id = thread
        .list_assets(session, dataset_id, None, 1)
        .unwrap()
        .items[0]
        .asset_id;
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
    let make_request = || {
        let mut context = context();
        context.idempotency_key = Some(id(Uuid::new_v4().to_string()));
        GeometrySubmitRequest {
            context,
            dataset_id: id(dataset_id.to_string()),
            asset_id: id(asset_id.to_string()),
        }
    };
    let interpreter = repo.join("target/dev/worker-venv/Scripts/python.exe");
    let entrypoint = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
    let runner = GeometryRunner::new(interpreter.clone(), entrypoint);
    let wait = |task_id: UuidText| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let task = commands
                .import_task(
                    &imports,
                    TaskGetRequest {
                        context: context(),
                        task_id: task_id.clone(),
                    },
                )
                .unwrap();
            if !matches!(task.status.as_str(), "QUEUED" | "DISPATCHING" | "RUNNING") {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "last geometry status: {}",
                task.status
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let operation = commands
        .start_geometry(Arc::clone(&runner), make_request())
        .unwrap();
    assert_eq!(operation.status, "QUEUED");
    let result = wait(operation.task_id.clone());
    assert_eq!(result.status, "SUCCEEDED");
    let report = result.geometry_report.unwrap();
    assert_eq!((report.width, report.height), (2, 1));
    assert_eq!(report.findings[0].code, "bbox_out_of_bounds");
    assert_eq!(report.findings[0].annotation_id, Some(7.into()));
    let completed = commands
        .request_task_cancellation(TaskCancelRequest {
            context: context(),
            task_id: operation.task_id,
        })
        .unwrap();
    assert!(!completed.requested);
    assert_eq!(completed.observed_status, "SUCCEEDED");

    let failed_entrypoint = run.join("worker-start-failure.py");
    fs::write(&failed_entrypoint, b"raise SystemExit(3)\n").unwrap();
    let runner = GeometryRunner::new(interpreter.clone(), failed_entrypoint);
    let failed = commands.start_geometry(runner, make_request()).unwrap();
    let result = wait(failed.task_id);
    assert_eq!(result.status, "FAILED");
    assert_eq!(result.error_code.as_deref(), Some("WORKER_START_FAILED"));

    let crash_entrypoint = run.join("geometry-worker-crash.py");
    fs::write(
        &crash_entrypoint,
        include_bytes!("geometry_worker_crash.py"),
    )
    .unwrap();
    let runner = GeometryRunner::new(interpreter, crash_entrypoint);
    let interrupted = commands.start_geometry(runner, make_request()).unwrap();
    let result = wait(interrupted.task_id);
    assert_eq!(result.status, "INTERRUPTED");
    assert_eq!(result.error_code.as_deref(), Some("WORKER_INTERRUPTED"));
    assert!(result.geometry_report.is_none());
    let queued = dd_infra::geometry_runtime::GeometryRuntime::admit(
        &thread,
        session,
        dd_infra::geometry_runtime::GeometryAuditRequest {
            dataset_id,
            asset_id,
            max_pixels: 100,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
        },
    )
    .unwrap();
    let task_id = id(queued.task().task_id.to_string());
    let cancelled = commands
        .request_task_cancellation(TaskCancelRequest {
            context: context(),
            task_id: task_id.clone(),
        })
        .unwrap();
    assert!(cancelled.requested);
    assert_eq!(cancelled.observed_status, "CANCELLED");
    let duplicate = commands
        .request_task_cancellation(TaskCancelRequest {
            context: context(),
            task_id: task_id.clone(),
        })
        .unwrap();
    assert_eq!(cancelled.revision, duplicate.revision);
    assert_eq!(
        commands
            .import_task(
                &imports,
                TaskGetRequest {
                    context: context(),
                    task_id: task_id.clone()
                }
            )
            .unwrap()
            .status,
        "CANCELLED"
    );
    let mut stale = context();
    stale.workspace_session_id = id(Uuid::new_v4().to_string());
    assert_eq!(
        commands
            .request_task_cancellation(TaskCancelRequest {
                context: stale,
                task_id,
            })
            .unwrap_err()
            .code,
        "STALE_WORKSPACE_SESSION"
    );
    thread.close(session).unwrap();
}
