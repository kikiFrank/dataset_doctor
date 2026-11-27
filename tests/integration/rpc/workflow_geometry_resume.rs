use std::{fs, sync::Arc};

use dd_infra::dataset_profile_runtime::DatasetProfileRuntime;
use dd_infra::geometry_runtime::GeometryRuntime;
use dd_infra::workflow_store::{
    LocalHealthcheckRuntime, WorkflowGeometryRequest, WorkflowReportRequest, WorkflowResumeRequest,
    WorkflowStatus,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const ANNOTATION: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn resume_keeps_the_accepted_geometry_attempt_and_dispatches_only_the_pending_job() {
    run_geometry_resume("Geometry Resume", false).await;
}

#[tokio::test]
async fn local_runtime_continues_a_resumed_geometry_batch_without_starting_it_again() {
    run_geometry_resume("Local Geometry Resume", true).await;
}

async fn run_geometry_resume(case: &str, local_runtime: bool) {
    let fixture = if local_runtime {
        WorkflowFixture::with_config(
            case,
            ANNOTATION,
            serde_json::json!({
                "plan_source":"LOCAL_DEFAULT", "sample_limit":2, "max_pixels":100,
            }),
        )
    } else {
        WorkflowFixture::new(case, ANNOTATION)
    };
    let owner = &fixture.workspace;
    let store = &fixture.store;
    let session = fixture.session;
    let started = store
        .start_profile(session, &fixture.policy, fixture.start_request(), 5)
        .unwrap();
    let run = started.workflow.run_id;
    let plan = started.workflow.plan_ref.unwrap();
    let mut profile = DatasetProfileRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        profile.run(started.job.unwrap()).await.unwrap().status,
        "SUCCEEDED"
    );
    profile.shutdown().await.unwrap();

    let submit = |expected_row_revision| WorkflowGeometryRequest {
        reference: plan.clone(),
        expected_row_revision,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        max_pixels: 100,
    };
    let admitted = store
        .start_geometry(
            session,
            &fixture.policy,
            submit(started.workflow.row_revision),
            6,
        )
        .unwrap();
    assert_eq!(admitted.jobs.len(), 2);
    let existing = admitted
        .workflow
        .geometry_tasks
        .iter()
        .map(|task| task.task_id)
        .collect::<Vec<_>>();
    let mut jobs = admitted.jobs.into_iter();
    let mut geometry = GeometryRuntime::launch(
        Arc::clone(owner),
        session,
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .await
    .unwrap();
    let first = geometry.audit_admitted(jobs.next().unwrap()).await.unwrap();
    assert_eq!(first.status, "SUCCEEDED");
    geometry.shutdown().await.unwrap();
    let completed_path = fixture.root.join(first.output_manifest_path.unwrap());
    let sealed = fs::read(&completed_path).unwrap();
    let paused = store
        .pause(session, run, admitted.workflow.row_revision, 7)
        .unwrap();
    assert_eq!(paused.observed_status, WorkflowStatus::Paused);
    owner.close(session).unwrap();
    drop(jobs);

    let directory = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(directory.grant_id).unwrap().workspace_session_id;
    let request = submit(paused.revision);
    fs::write(&completed_path, b"damaged accepted geometry report").unwrap();
    assert!(
        store
            .resume_geometry(reopened, &fixture.policy, request.clone(), 8)
            .is_err()
    );
    fs::write(&completed_path, &sealed).unwrap();
    assert_eq!(
        store.get(reopened, run).unwrap().status,
        WorkflowStatus::Paused
    );
    assert!(
        store
            .resume_geometry(
                reopened,
                &fixture.policy,
                WorkflowGeometryRequest {
                    max_pixels: 99,
                    ..request.clone()
                },
                8,
            )
            .is_err()
    );
    assert_eq!(
        store.get(reopened, run).unwrap().row_revision,
        paused.revision
    );

    let remaining = *existing.iter().find(|id| **id != first.task_id).unwrap();
    let report = if local_runtime {
        let runtime = LocalHealthcheckRuntime::new(
            Arc::clone(owner),
            fixture.interpreter,
            fixture.entrypoint,
        )
        .unwrap();
        let resumed = runtime
            .resume(
                reopened,
                WorkflowResumeRequest {
                    reference: plan,
                    expected_row_revision: paused.revision,
                },
                Uuid::new_v4(),
            )
            .unwrap();
        assert_eq!(resumed.workflow.run_id, run);
        assert_eq!(resumed.workflow.status, WorkflowStatus::Running);
        runtime.run(resumed.job.unwrap()).await.unwrap()
    } else {
        let resumed = store
            .resume_geometry(reopened, &fixture.policy, request, 9)
            .unwrap();
        assert_eq!(resumed.workflow.run_id, run);
        assert_eq!(resumed.workflow.status, WorkflowStatus::Running);
        assert_eq!(
            resumed
                .workflow
                .geometry_tasks
                .iter()
                .map(|task| task.task_id)
                .collect::<Vec<_>>(),
            existing
        );
        assert_eq!(resumed.jobs.len(), 1);
        let mut geometry = GeometryRuntime::launch(
            Arc::clone(owner),
            reopened,
            fixture.interpreter,
            fixture.entrypoint,
        )
        .await
        .unwrap();
        let second = geometry
            .audit_admitted(resumed.jobs.into_iter().next().unwrap())
            .await
            .unwrap();
        assert_eq!(second.status, "SUCCEEDED");
        assert_eq!(second.task_id, remaining);
        geometry.shutdown().await.unwrap();
        store
            .build_report(
                reopened,
                &fixture.policy,
                WorkflowReportRequest {
                    reference: plan,
                    expected_row_revision: resumed.workflow.row_revision,
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                },
                10,
            )
            .unwrap()
    };
    assert_eq!(report.data.scope.dataset_id, fixture.dataset);
    assert_eq!(report.data.findings_by_severity.get("ERROR"), Some(&1));
    assert_eq!(
        store.get(reopened, run).unwrap().status,
        WorkflowStatus::WaitingReview
    );
    assert_eq!(fs::read(completed_path).unwrap(), sealed);
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        ANNOTATION
    );
    owner.close(reopened).unwrap();
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    for (task_id, task_session) in [(first.task_id, session), (remaining, reopened)] {
        let attempt = db
            .query_row(
                "SELECT attempt_no,workspace_session_id FROM tasks WHERE task_id=?1",
                [task_id.to_string()],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(attempt, (1, task_session.to_string()));
    }
}
