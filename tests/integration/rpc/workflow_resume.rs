use std::{fs, sync::Arc};

use dd_infra::dataset_profile_runtime::DatasetProfileRuntime;
use dd_infra::geometry_runtime::GeometryRuntime;
use dd_infra::workflow_store::{
    WorkflowError, WorkflowGeometryRequest, WorkflowReportRequest, WorkflowResumeRequest,
    WorkflowStatus,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const ANNOTATION: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn resume_reopens_a_verified_profile_and_continues_to_real_geometry_and_report() {
    let fixture = WorkflowFixture::new("Workflow Resume", ANNOTATION);
    let owner = &fixture.workspace;
    let store = &fixture.store;
    let session = fixture.session;
    let started = store
        .start_profile(session, &fixture.policy, fixture.start_request(), 5)
        .unwrap();
    let run = started.workflow.run_id;
    let plan = started.workflow.plan_ref.unwrap();
    let profile_task = started.workflow.profile_task.unwrap().task_id;
    let mut profile = DatasetProfileRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .await
    .unwrap();
    let accepted = profile.run(started.job.unwrap()).await.unwrap();
    assert_eq!(accepted.status, "SUCCEEDED");
    profile.shutdown().await.unwrap();
    let result_path = fixture.root.join(accepted.output_manifest_path.unwrap());
    let sealed = fs::read(&result_path).unwrap();
    let pause = store
        .pause(session, run, started.workflow.row_revision, 6)
        .unwrap();
    assert_eq!(pause.observed_status, WorkflowStatus::Paused);

    owner.close(session).unwrap();
    let directory = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(directory.grant_id).unwrap().workspace_session_id;
    let request = WorkflowResumeRequest {
        reference: plan.clone(),
        expected_row_revision: pause.revision,
    };
    assert!(
        store
            .resume(session, &fixture.policy, request.clone(), 7)
            .is_err()
    );
    assert!(matches!(
        store.resume(
            reopened,
            &fixture.policy,
            WorkflowResumeRequest {
                expected_row_revision: pause.revision - 1,
                ..request.clone()
            },
            7
        ),
        Err(WorkflowError::RevisionConflict)
    ));
    let annotation = fixture.source.join("signal.json");
    fs::write(&annotation, b"changed before resume").unwrap();
    assert!(matches!(
        store.resume(reopened, &fixture.policy, request.clone(), 7),
        Err(WorkflowError::InputChanged)
    ));
    fs::write(&annotation, ANNOTATION).unwrap();
    fs::write(&result_path, b"changed sealed profile").unwrap();
    assert!(
        store
            .resume(reopened, &fixture.policy, request.clone(), 7)
            .is_err()
    );
    fs::write(&result_path, &sealed).unwrap();
    let still_paused = store.get(reopened, run).unwrap();
    assert_eq!(still_paused.status, WorkflowStatus::Paused);
    assert_eq!(still_paused.row_revision, pause.revision);

    let resumed = std::thread::scope(|scope| {
        let request_one = request.clone();
        let policy = &fixture.policy;
        let first = scope.spawn(move || store.resume(reopened, policy, request_one, 8));
        let second = scope.spawn(move || store.resume(reopened, policy, request, 8));
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        match (first, second) {
            (Ok(resumed), Err(WorkflowError::RevisionConflict))
            | (Err(WorkflowError::RevisionConflict), Ok(resumed)) => resumed,
            other => panic!("exactly one resume may advance the revision: {other:?}"),
        }
    });
    assert_eq!(resumed.run_id, run);
    assert_eq!(resumed.row_revision, pause.revision + 1);
    assert_eq!(resumed.plan_ref, Some(plan.clone()));
    assert_eq!(resumed.status, WorkflowStatus::Running);
    assert_eq!(resumed.profile_task.unwrap().task_id, profile_task);
    let geometry = store
        .start_geometry(
            reopened,
            &fixture.policy,
            WorkflowGeometryRequest {
                reference: plan.clone(),
                expected_row_revision: resumed.row_revision,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                max_pixels: 100,
            },
            9,
        )
        .unwrap();
    assert_eq!(geometry.jobs.len(), 2);
    let mut worker = GeometryRuntime::launch(
        Arc::clone(owner),
        reopened,
        fixture.interpreter,
        fixture.entrypoint,
    )
    .await
    .unwrap();
    for job in geometry.jobs {
        assert_eq!(
            worker.audit_admitted(job).await.unwrap().status,
            "SUCCEEDED"
        );
    }
    worker.shutdown().await.unwrap();
    let report = store
        .build_report(
            reopened,
            &fixture.policy,
            WorkflowReportRequest {
                reference: plan,
                expected_row_revision: geometry.workflow.row_revision,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
            10,
        )
        .unwrap();
    assert_eq!(report.data.scope.workflow_run_id, run);
    assert_eq!(report.data.scope.dataset_id, fixture.dataset);
    assert_eq!(report.data.findings_by_severity.get("ERROR"), Some(&1));
    let waiting = store.get(reopened, run).unwrap();
    assert_eq!(waiting.status, WorkflowStatus::WaitingReview);
    assert_eq!(fs::read(annotation).unwrap(), ANNOTATION);
    assert_eq!(fs::read(result_path).unwrap(), sealed);
    owner.close(reopened).unwrap();

    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(db.query_row(
        "SELECT count(*) FROM tasks WHERE workflow_run_id=?1 AND capability_id='dataset_profile.v1'",
        [run.to_string()], |row| row.get::<_, i64>(0),
    ).unwrap(), 1);
    assert_eq!(
        db.query_row(
            "SELECT attempt_no FROM tasks WHERE task_id=?1",
            [profile_task.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM review_sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
