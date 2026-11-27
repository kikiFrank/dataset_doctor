use std::{fs, sync::Arc};

use dd_infra::dataset_profile_runtime::{DatasetProfileRuntime, read_dataset_profile};
use dd_infra::geometry_runtime::GeometryRuntime;
use dd_infra::workflow_store::{
    LocalHealthcheckRequest, LocalHealthcheckRuntime, WorkflowError, WorkflowGeometryRequest,
    WorkflowReportRequest, WorkflowStatus,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const ANNOTATION: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn a_paused_local_runner_leaves_the_queued_profile_available_for_resume() {
    let fixture = WorkflowFixture::new("Paused Local Runner", ANNOTATION);
    let runtime = LocalHealthcheckRuntime::new(
        Arc::clone(&fixture.workspace),
        fixture.interpreter,
        fixture.entrypoint,
    )
    .unwrap();
    let started = runtime
        .admit(
            fixture.session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 2,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let run = started.workflow.run_id;
    let paused = fixture
        .store
        .pause(fixture.session, run, started.workflow.row_revision, 20)
        .unwrap();
    assert_eq!(paused.observed_status, WorkflowStatus::Paused);
    assert!(matches!(
        runtime.run(started.job.unwrap()).await,
        Err(WorkflowError::Paused)
    ));
    let state = fixture.store.get(fixture.session, run).unwrap();
    assert_eq!(state.status, WorkflowStatus::Paused);
    assert_eq!(state.row_revision, paused.revision);
    assert_eq!(state.profile_task.unwrap().status, "QUEUED");
    assert!(state.error_code.is_none());
    fixture.workspace.close(fixture.session).unwrap();
}

#[tokio::test]
async fn pause_at_the_real_profile_boundary_blocks_geometry_and_survives_reopen() {
    let fixture = WorkflowFixture::new("Workflow Pause", ANNOTATION);
    let owner = &fixture.workspace;
    let store = &fixture.store;
    let session = fixture.session;
    assert!(matches!(
        store.pause(session, fixture.ready.run_id, fixture.ready.row_revision, 5),
        Err(WorkflowError::ControlUnavailable)
    ));
    let accepted = store
        .start_profile(session, &fixture.policy, fixture.start_request(), 5)
        .unwrap();
    let run = accepted.workflow.run_id;
    let revision = accepted.workflow.row_revision;
    let task = accepted.workflow.profile_task.unwrap();
    assert_eq!(task.status, "QUEUED");
    assert!(matches!(
        store.pause(session, run, revision - 1, 10),
        Err(WorkflowError::RevisionConflict)
    ));
    let mut profiler = DatasetProfileRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter,
        fixture.entrypoint,
    )
    .await
    .unwrap();
    let result = profiler.run(accepted.job.unwrap()).await.unwrap();
    assert_eq!(result.status, "SUCCEEDED");
    profiler.shutdown().await.unwrap();
    {
        let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER force_pause_failure BEFORE UPDATE OF status ON workflow_runs
             WHEN NEW.status='PAUSED'
             BEGIN SELECT RAISE(ABORT, 'synthetic pause write failure'); END;",
        )
        .unwrap();
        assert!(matches!(
            store.pause(session, run, revision, 9),
            Err(WorkflowError::Storage(_))
        ));
        let unchanged = store.get(session, run).unwrap();
        assert_eq!(unchanged.status, WorkflowStatus::Running);
        assert_eq!(unchanged.row_revision, revision);
        db.execute_batch("DROP TRIGGER force_pause_failure")
            .unwrap();
    }
    let receipt = store.pause(session, run, revision, 10).unwrap();
    assert_eq!(receipt.observed_status, WorkflowStatus::Paused);
    assert!(receipt.blocking_task_ids.is_empty());
    assert_eq!(receipt.revision, revision + 1);
    let geometry = WorkflowGeometryRequest {
        reference: accepted.workflow.plan_ref.unwrap(),
        expected_row_revision: receipt.revision,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        max_pixels: 100,
    };
    assert!(matches!(
        store.start_geometry(session, &fixture.policy, geometry.clone(), 11),
        Err(WorkflowError::Paused)
    ));
    assert!(matches!(
        store.build_report(
            session,
            &fixture.policy,
            WorkflowReportRequest {
                reference: geometry.reference.clone(),
                expected_row_revision: receipt.revision,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
            11,
        ),
        Err(WorkflowError::Paused)
    ));

    let settled = store.get(session, run).unwrap();
    assert_eq!(settled.status, WorkflowStatus::Paused);
    assert_eq!(settled.row_revision, receipt.revision);
    assert!(settled.geometry_tasks.is_empty());
    assert!(settled.report_task_id.is_none());
    assert!(settled.error_code.is_none());
    assert_eq!(settled.profile_task.unwrap().status, "SUCCEEDED");
    read_dataset_profile(owner, session, fixture.dataset, task.task_id).unwrap();
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        ANNOTATION
    );

    owner.close(session).unwrap();
    {
        let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let mut violations = db.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(!violations.exists([]).unwrap());
        let kinds = db
            .prepare("SELECT node_id FROM tasks WHERE workflow_run_id=?1 ORDER BY node_id")
            .unwrap()
            .query_map([run.to_string()], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            kinds.len(),
            2,
            "only the planning and actual profile tasks may exist"
        );
    }
    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let reopened_run = store.get(reopened, run).unwrap();
    assert_eq!(reopened_run.status, WorkflowStatus::Paused);
    assert_eq!(reopened_run.row_revision, settled.row_revision);
    read_dataset_profile(owner, reopened, fixture.dataset, task.task_id).unwrap();
    assert!(matches!(
        store.start_geometry(reopened, &fixture.policy, geometry, 12),
        Err(WorkflowError::Paused)
    ));
    owner.close(reopened).unwrap();
}

#[tokio::test]
async fn a_real_worker_cannot_start_an_existing_queued_geometry_job_while_paused() {
    let fixture = WorkflowFixture::new("Workflow Pause Dispatch", ANNOTATION);
    let owner = &fixture.workspace;
    let store = &fixture.store;
    let session = fixture.session;
    let started = store
        .start_profile(session, &fixture.policy, fixture.start_request(), 5)
        .unwrap();
    let run = started.workflow.run_id;
    let mut profiler = DatasetProfileRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        profiler.run(started.job.unwrap()).await.unwrap().status,
        "SUCCEEDED"
    );
    profiler.shutdown().await.unwrap();
    let accepted = store
        .start_geometry(
            session,
            &fixture.policy,
            WorkflowGeometryRequest {
                reference: started.workflow.plan_ref.unwrap(),
                expected_row_revision: started.workflow.row_revision,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                max_pixels: 100,
            },
            6,
        )
        .unwrap();
    assert_eq!(accepted.jobs.len(), 2);
    let jobs = accepted.workflow.geometry_tasks;
    assert!(jobs.iter().all(|job| job.status == "QUEUED"));
    let paused = store
        .pause(session, run, accepted.workflow.row_revision, 7)
        .unwrap();
    assert_eq!(paused.observed_status, WorkflowStatus::Paused);
    assert!(paused.blocking_task_ids.is_empty());

    let mut worker = GeometryRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter,
        fixture.entrypoint,
    )
    .await
    .unwrap();
    for job in accepted.jobs {
        let dispatched = worker.audit_admitted(job).await;
        assert!(
            dispatched.is_err(),
            "a queued job crossed the persisted pause boundary"
        );
    }
    worker.shutdown().await.unwrap();
    let still_paused = store.get(session, run).unwrap();
    assert_eq!(still_paused.status, WorkflowStatus::Paused);
    assert_eq!(still_paused.row_revision, paused.revision);
    assert!(
        still_paused
            .geometry_tasks
            .iter()
            .all(|job| job.status == "QUEUED")
    );
    owner.close(session).unwrap();
    let database = Connection::open(fixture.root.join("workspace.db")).unwrap();
    for job in jobs {
        let attempts = database.query_row(
            "SELECT attempt_no, current_attempt_id, output_manifest_path FROM tasks WHERE task_id=?1",
            [job.task_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?)),
        ).unwrap();
        assert_eq!(attempts, (0, None, None));
    }
}
