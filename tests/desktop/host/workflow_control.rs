use std::{fs, time::Instant};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::workflow_store::{LocalHealthcheckRequest, LocalHealthcheckRuntime};
use dd_ui_contracts::workflow::{
    WorkflowControlAction, WorkflowGetRequest, WorkflowPauseRequest, WorkflowResumeRequest,
    WorkflowResumeStrategy, WorkflowStatusView,
};

use super::*;

#[test]
fn desktop_pauses_and_resumes_the_same_real_healthcheck_after_reopen() {
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
    let root = run.join("Desktop Workflow Controls");
    let source = run.join("Desktop Workflow Control Sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("signal.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    let annotation = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;
    let annotation_path = source.join("signal.json");
    fs::write(&annotation_path, annotation).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let root_grant = owner.grant_selected_directory(root.clone()).unwrap();
    let workspace = owner
        .create(root_grant.grant_id, "Workflow controls".into(), 1)
        .unwrap();
    let session = workspace.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner.grant_import_directory(session, source).unwrap();
    let preflight = imports
        .inspect_directory(session, grant.grant_id, "Synthetic controls".into())
        .unwrap();
    let dataset = imports
        .confirm(session, preflight.plan_id, preflight.fingerprint, 2)
        .unwrap()
        .dataset_id
        .parse()
        .unwrap();
    let runtime = Arc::new(
        LocalHealthcheckRuntime::new(
            Arc::clone(&owner),
            repo.join("target/dev/worker-venv/Scripts/python.exe"),
            repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
        )
        .unwrap(),
    );
    let accepted = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: dataset,
                sample_limit: 1,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let initial_task = accepted.workflow.profile_task.unwrap().task_id;
    let id = |value: Uuid| UuidText::try_from(value.to_string()).unwrap();
    let run_id = id(accepted.workflow.run_id);
    let context = |current| CommandContext {
        request_id: id(Uuid::new_v4()),
        workspace_id: UuidText::try_from(workspace.workspace_id.clone()).unwrap(),
        workspace_session_id: id(current),
        idempotency_key: None,
    };
    let commands = Commands {
        thread: Arc::clone(&owner),
        request_id: id(Uuid::new_v4()),
    };
    let receipt = commands
        .pause_workflow(WorkflowPauseRequest {
            context: context(session),
            workflow_run_id: run_id.clone(),
            expected_revision: accepted.workflow.row_revision.into(),
        })
        .unwrap();
    assert_eq!(receipt.requested, WorkflowControlAction::Pause);
    assert_eq!(receipt.observed_status, WorkflowStatusView::Paused);
    assert!(receipt.blocking_task_ids.is_empty());
    drop(accepted.job);
    owner.close(session).unwrap();

    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let resume = |current| WorkflowResumeRequest {
        context: context(current),
        workflow_run_id: run_id.clone(),
        expected_revision: receipt.revision,
        strategy: WorkflowResumeStrategy::ResumePaused,
    };
    assert!(
        commands
            .resume_workflow(Arc::clone(&runtime), resume(session))
            .is_err()
    );
    let resumed = commands
        .resume_workflow(Arc::clone(&runtime), resume(reopened))
        .unwrap();
    assert_eq!(resumed.requested, WorkflowControlAction::Resume);
    assert_eq!(resumed.observed_status, WorkflowStatusView::Running);
    assert!(commands.resume_workflow(runtime, resume(reopened)).is_err());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let current = commands
            .get_workflow(WorkflowGetRequest {
                context: context(reopened),
                workflow_run_id: run_id.clone(),
            })
            .unwrap();
        if current.status == WorkflowStatusView::WaitingReview {
            assert_eq!(current.profile_task.unwrap().task_id, id(initial_task));
            assert_eq!(current.geometry_tasks.len(), 1);
            assert!(current.report_task_id.is_some());
            break;
        }
        assert_eq!(current.status, WorkflowStatusView::Running, "{current:?}");
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(fs::read(annotation_path).unwrap(), annotation);
    owner.close(reopened).unwrap();
}
