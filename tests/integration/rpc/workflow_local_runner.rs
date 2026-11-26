use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::workflow_store::{
    LocalHealthcheckRequest, LocalHealthcheckRuntime, PlanSource, WorkflowError, WorkflowStatus,
    WorkflowStore,
};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const COCO: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn local_healthcheck_starts_from_the_dataset_and_runs_to_a_real_report() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        run.parent(),
        Some(repo.join("target/contract-check/worker-rpc").as_path())
    );
    let root = run.join("Local Healthcheck Workspace");
    let source = run.join("Local Healthcheck Sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("signal.png"), PNG).unwrap();
    fs::write(source.join("signal.json"), COCO).unwrap();
    fs::write(source.join("unannotated.png"), PNG).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Local Healthcheck".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let preview = imports
        .inspect_directory(session, grant.grant_id, "Registered samples".into())
        .unwrap();
    let dataset = imports
        .confirm(session, preview.plan_id, preview.fingerprint, 2)
        .unwrap()
        .dataset_id
        .parse()
        .unwrap();
    let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entrypoint = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
    let runtime =
        LocalHealthcheckRuntime::new(Arc::clone(&owner), interpreter, entrypoint.clone()).unwrap();
    let request = LocalHealthcheckRequest {
        dataset_id: dataset,
        sample_limit: 2,
        max_pixels: 100,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };

    let accepted = runtime.admit(session, request.clone()).unwrap();
    let run_id = accepted.workflow.run_id;
    assert_eq!(accepted.workflow.status, WorkflowStatus::Running);
    assert_eq!(
        accepted.workflow.plan_source,
        Some(PlanSource::LocalDefault)
    );
    assert_eq!(accepted.workflow.profile_task.unwrap().status, "QUEUED");
    let replay = runtime.admit(session, request.clone()).unwrap();
    assert_eq!(replay.workflow.run_id, run_id);
    assert!(replay.job.is_none());
    assert!(matches!(
        runtime.admit(
            session,
            LocalHealthcheckRequest {
                sample_limit: 1,
                ..request.clone()
            }
        ),
        Err(WorkflowError::IdempotencyConflict)
    ));
    let report = runtime.run(accepted.job.unwrap()).await.unwrap();
    assert_eq!(report.data.scope.workflow_run_id, run_id);
    assert_eq!(report.data.scope.dataset_id, dataset);
    assert_eq!(report.data.findings_by_severity.get("ERROR"), Some(&1));
    assert!(
        report
            .data
            .unexecuted_checks
            .contains(&"semantic_review".into())
    );
    assert_eq!(fs::read(source.join("signal.json")).unwrap(), COCO);
    let store = WorkflowStore::new(Arc::clone(&owner)).unwrap();
    assert_eq!(
        store.get(session, run_id).unwrap().status,
        WorkflowStatus::WaitingReview
    );
    assert!(
        runtime
            .admit(session, request.clone())
            .unwrap()
            .job
            .is_none()
    );

    owner.close(session).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let successful = db
        .query_row(
            "SELECT count(*) FROM tasks WHERE workflow_run_id=?1 AND status='SUCCEEDED'",
            [run_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(successful, 5);
    drop(db);
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        store.read_report(reopened, run_id).unwrap().reference,
        report.reference
    );

    let unavailable = LocalHealthcheckRuntime::new(
        Arc::clone(&owner),
        root.join("unavailable-python.exe"),
        entrypoint,
    )
    .unwrap();
    let queued = unavailable
        .admit(
            reopened,
            LocalHealthcheckRequest {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                ..request
            },
        )
        .unwrap();
    let failed = queued.workflow.run_id;
    let failed_task = queued.workflow.profile_task.unwrap().task_id;
    assert!(unavailable.run(queued.job.unwrap()).await.is_err());
    let failure = store.get(reopened, failed).unwrap();
    assert_eq!(failure.status, WorkflowStatus::Failed);
    assert_eq!(failure.error_code.as_deref(), Some("WORKER_START_FAILED"));
    assert!(failure.report_task_id.is_none());
    let recorded = owner.task_outcome(reopened, failed_task).unwrap();
    assert_eq!(recorded.status, "FAILED");
    assert_eq!(recorded.error_code.as_deref(), Some("WORKER_START_FAILED"));
    owner.close(reopened).unwrap();
}
