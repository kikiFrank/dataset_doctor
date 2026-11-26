use std::{fs, path::PathBuf, time::Instant};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::workflow_store::LocalHealthcheckRuntime;
use dd_ui_contracts::workflow::{
    ReportGetRequest, WorkflowDatasetScope, WorkflowGetRequest, WorkflowPolicyOverrides,
    WorkflowReportGetRequest, WorkflowStartRequest, WorkflowStatusView,
};

use super::*;

#[test]
fn desktop_starts_a_real_local_healthcheck_and_reopens_its_human_review_report() {
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
    let root = run.join("Desktop Workflow Workspace");
    let source = run.join("Desktop Workflow Sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("signal.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    let labels = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;
    fs::write(source.join("signal.json"), labels).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let workspace = owner.create(grant.grant_id, "Workflow".into(), 1).unwrap();
    let session = workspace.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let inspected = imports
        .inspect_directory(session, grant.grant_id, "Registered samples".into())
        .unwrap();
    let registered = imports
        .confirm(session, inspected.plan_id, inspected.fingerprint, 2)
        .unwrap();
    let id = |text: String| UuidText::try_from(text).unwrap();
    let key = id(Uuid::new_v4().to_string());
    let context = |current: Uuid| CommandContext {
        request_id: id(Uuid::new_v4().to_string()),
        workspace_id: id(workspace.workspace_id.clone()),
        workspace_session_id: id(current.to_string()),
        idempotency_key: Some(key.clone()),
    };
    let mut request = WorkflowStartRequest {
        context: context(session),
        scope: WorkflowDatasetScope {
            dataset_id: id(registered.dataset_id),
        },
        template_id: "unregistered.v1".into(),
        policy_overrides: WorkflowPolicyOverrides {
            sample_limit: Some(1),
        },
    };
    let commands = Commands {
        thread: Arc::clone(&owner),
        request_id: id(Uuid::new_v4().to_string()),
    };
    let runtime = Arc::new(
        LocalHealthcheckRuntime::new(
            Arc::clone(&owner),
            repo.join("target/dev/worker-venv/Scripts/python.exe"),
            repo.join("services/python-worker/src/dd_worker/entrypoint.py"),
        )
        .unwrap(),
    );
    assert!(
        commands
            .start_workflow(Arc::clone(&runtime), request.clone())
            .is_err()
    );
    request.template_id = "industrial_dataset_healthcheck.v1".into();
    let accepted = commands
        .start_workflow(Arc::clone(&runtime), request.clone())
        .unwrap();
    assert_eq!(accepted.status, WorkflowStatusView::Running);
    assert!(accepted.profile_task.is_some());
    let run_id = accepted.workflow_run_id;
    assert_eq!(
        commands
            .start_workflow(runtime, request)
            .unwrap()
            .workflow_run_id,
        run_id
    );
    let read = |current| {
        commands.get_workflow(WorkflowGetRequest {
            context: context(current),
            workflow_run_id: run_id.clone(),
        })
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let finished = loop {
        let current = read(session).unwrap();
        if current.status != WorkflowStatusView::Running {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "workflow did not settle: {current:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(finished.status, WorkflowStatusView::WaitingReview);
    assert_eq!(finished.geometry_tasks.len(), 1);
    assert_eq!(finished.geometry_tasks[0].status, "SUCCEEDED");
    assert!(finished.report_task_id.is_some());
    assert!(matches!(
        serde_json::from_value::<ReportGetRequest>(serde_json::json!({
            "context":context(session), "workflow_run_id":run_id,
        }))
        .unwrap(),
        ReportGetRequest::Workflow(_)
    ));
    assert!(matches!(
        serde_json::from_value::<ReportGetRequest>(serde_json::json!({
            "context":context(session), "task_id":finished.report_task_id,
        }))
        .unwrap(),
        ReportGetRequest::Staging(_)
    ));
    assert!(
        serde_json::from_value::<ReportGetRequest>(serde_json::json!({
            "context":context(session), "workflow_run_id":run_id, "task_id":finished.report_task_id,
        }))
        .is_err()
    );
    let read_report = |current| {
        commands.get_workflow_report(WorkflowReportGetRequest {
            context: context(current),
            workflow_run_id: run_id.clone(),
        })
    };
    let report = read_report(session).unwrap();
    assert!(
        report
            .checks
            .iter()
            .flat_map(|check| &check.findings)
            .any(|finding| finding.finding_code == "bbox_out_of_bounds")
    );
    assert!(report.unexecuted_checks.contains(&"semantic_review".into()));
    assert_eq!(fs::read(source.join("signal.json")).unwrap(), labels);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(read(session).is_err());
    assert_eq!(
        read(reopened).unwrap().status,
        WorkflowStatusView::WaitingReview
    );
    assert_eq!(read_report(reopened).unwrap().report_id, report.report_id);
    owner.close(reopened).unwrap();
}
