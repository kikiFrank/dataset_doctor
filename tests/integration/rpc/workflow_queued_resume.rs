use std::{fs, sync::Arc};

use dd_infra::workflow_store::{
    LocalHealthcheckRequest, LocalHealthcheckRuntime, WorkflowError, WorkflowResumeRequest,
    WorkflowStatus,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const ANNOTATION: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn local_resume_reopens_and_executes_the_original_queued_profile_once() {
    let fixture = WorkflowFixture::new("Resume Queued Profile", ANNOTATION);
    let owner = &fixture.workspace;
    let store = &fixture.store;
    let session = fixture.session;
    let runtime =
        LocalHealthcheckRuntime::new(Arc::clone(owner), fixture.interpreter, fixture.entrypoint)
            .unwrap();
    let first = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 2,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let run = first.workflow.run_id;
    let task = first.workflow.profile_task.unwrap();
    assert_eq!(task.status, "QUEUED");
    let pause = store
        .pause(session, run, first.workflow.row_revision, 10)
        .unwrap();
    assert_eq!(pause.observed_status, WorkflowStatus::Paused);
    assert!(matches!(
        runtime.run(first.job.unwrap()).await,
        Err(WorkflowError::Paused)
    ));
    let unrelated = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 1,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap()
        .workflow;
    store
        .pause(session, unrelated.run_id, unrelated.row_revision, 11)
        .unwrap();
    owner.close(session).unwrap();

    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert_ne!(reopened, session);
    let paused = store.get(reopened, run).unwrap();
    assert_eq!(paused.status, WorkflowStatus::Paused);
    let request = WorkflowResumeRequest {
        reference: paused.plan_ref.unwrap(),
        expected_row_revision: pause.revision,
    };
    let source = fixture.source.join("signal.json");
    fs::write(&source, b"external change during pause").unwrap();
    assert!(matches!(
        runtime.resume(reopened, request.clone(), Uuid::new_v4()),
        Err(WorkflowError::InputChanged)
    ));
    fs::write(&source, ANNOTATION).unwrap();
    {
        let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_profile_rebind BEFORE UPDATE OF workspace_session_id ON tasks
             WHEN OLD.capability_id='dataset_profile.v1' AND OLD.status='QUEUED'
             BEGIN SELECT RAISE(ABORT, 'synthetic profile rebind failure'); END;",
        )
        .unwrap();
        assert!(matches!(
            runtime.resume(reopened, request.clone(), Uuid::new_v4()),
            Err(WorkflowError::Storage(_))
        ));
        let rolled_back = store.get(reopened, run).unwrap();
        assert_eq!(rolled_back.status, WorkflowStatus::Paused);
        assert_eq!(rolled_back.row_revision, pause.revision);
        assert_eq!(rolled_back.profile_task.unwrap().status, "QUEUED");
        db.execute_batch("DROP TRIGGER fail_profile_rebind")
            .unwrap();
    }
    let resumed = runtime
        .resume(reopened, request.clone(), Uuid::new_v4())
        .unwrap();
    assert_eq!(resumed.workflow.run_id, run);
    assert_eq!(
        resumed.workflow.profile_task.as_ref().unwrap().task_id,
        task.task_id
    );
    assert!(matches!(
        runtime.resume(reopened, request, Uuid::new_v4()),
        Err(WorkflowError::RevisionConflict)
    ));
    let report = runtime.run(resumed.job.unwrap()).await.unwrap();
    assert_eq!(report.data.scope.dataset_id, fixture.dataset);
    assert_eq!(report.data.scope.workflow_run_id, run);
    assert_eq!(report.data.findings_by_severity.get("ERROR"), Some(&1));
    assert_eq!(
        store.get(reopened, run).unwrap().status,
        WorkflowStatus::WaitingReview
    );
    let other = store.get(reopened, unrelated.run_id).unwrap();
    assert_eq!(other.status, WorkflowStatus::Paused);
    assert_eq!(other.profile_task.unwrap().status, "QUEUED");
    assert_eq!(fs::read(source).unwrap(), ANNOTATION);
    owner.close(reopened).unwrap();

    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let history = db
        .query_row(
            "SELECT workspace_session_id, status, attempt_no FROM tasks WHERE task_id=?1",
            [task.task_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(history, (reopened.to_string(), "SUCCEEDED".into(), 1));
    assert_eq!(db.query_row(
        "SELECT count(*) FROM tasks WHERE workflow_run_id=?1 AND capability_id='dataset_profile.v1'",
        [run.to_string()], |row| row.get::<_, i64>(0),
    ).unwrap(), 1);
}
