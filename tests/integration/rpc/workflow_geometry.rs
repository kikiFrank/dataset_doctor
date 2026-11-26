use std::{fs, sync::Arc};

use dd_infra::dataset_profile_runtime::DatasetProfileRuntime;
use dd_infra::geometry_runtime::{GeometryRuntime, read_geometry_report};
use dd_infra::workflow_store::{WorkflowError, WorkflowGeometryRequest, WorkflowStatus};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const COCO: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn accepted_profile_unlocks_real_geometry_for_its_current_plan_members() {
    let f = WorkflowFixture::new("Geometry Workflow", COCO);
    let started = f
        .store
        .start_profile(f.session, &f.policy, f.start_request(), 5)
        .unwrap();
    let run = started.workflow.run_id;
    let request = WorkflowGeometryRequest {
        reference: started.workflow.plan_ref.unwrap(),
        expected_row_revision: started.workflow.row_revision,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        max_pixels: 100,
    };
    assert!(matches!(
        f.store
            .start_geometry(f.session, &f.policy, request.clone(), 6),
        Err(WorkflowError::UpstreamNotReady)
    ));
    let mut profiler = DatasetProfileRuntime::launch(
        Arc::clone(&f.workspace),
        f.session,
        f.interpreter.clone(),
        f.entrypoint.clone(),
    )
    .await
    .unwrap();
    let profile = profiler.run(started.job.unwrap()).await.unwrap();
    assert_eq!(profile.status, "SUCCEEDED");
    profiler.shutdown().await.unwrap();
    let summary = f
        .root
        .join(profile.output_manifest_path.unwrap())
        .with_file_name("dataset-profile.json");
    let sealed = fs::read(&summary).unwrap();
    fs::write(&summary, b"{}").unwrap();
    assert!(
        f.store
            .start_geometry(f.session, &f.policy, request.clone(), 7)
            .is_err()
    );
    assert!(
        f.store
            .get(f.session, run)
            .unwrap()
            .geometry_tasks
            .is_empty()
    );
    fs::write(summary, sealed).unwrap();
    let annotation = f.source.join("signal.json");
    fs::write(&annotation, b"changed source JSON").unwrap();
    assert!(matches!(
        f.store
            .start_geometry(f.session, &f.policy, request.clone(), 8),
        Err(WorkflowError::InputChanged)
    ));
    fs::write(&annotation, COCO).unwrap();

    let db = Connection::open(f.root.join("workspace.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_geometry_group BEFORE UPDATE OF node_states_json ON workflow_runs
        BEGIN SELECT RAISE(ABORT, 'synthetic geometry admission failure'); END;",
    )
    .unwrap();
    assert!(
        f.store
            .start_geometry(f.session, &f.policy, request.clone(), 9)
            .is_err()
    );
    let unchanged = f.store.get(f.session, run).unwrap();
    assert_eq!(unchanged.row_revision, request.expected_row_revision);
    assert!(unchanged.geometry_tasks.is_empty());
    assert_eq!(
        db.query_row("SELECT count(*) FROM tasks", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    db.execute_batch("DROP TRIGGER fail_geometry_group")
        .unwrap();
    drop(db);

    let admitted = f
        .store
        .start_geometry(f.session, &f.policy, request.clone(), 9)
        .unwrap();
    assert_eq!(admitted.workflow.status, WorkflowStatus::Running);
    assert_eq!(
        admitted.workflow.row_revision,
        request.expected_row_revision + 1
    );
    assert_eq!(admitted.jobs.len(), 2);
    assert_eq!(admitted.workflow.geometry_tasks.len(), 2);
    assert!(
        admitted
            .workflow
            .geometry_tasks
            .iter()
            .all(|task| task.status == "QUEUED")
    );
    let replay = f
        .store
        .start_geometry(f.session, &f.policy, request, 10)
        .unwrap();
    assert!(replay.jobs.is_empty());
    assert_eq!(
        replay.workflow.geometry_tasks,
        admitted.workflow.geometry_tasks
    );

    let mut geometry = GeometryRuntime::launch(
        Arc::clone(&f.workspace),
        f.session,
        f.interpreter,
        f.entrypoint,
    )
    .await
    .unwrap();
    for job in admitted.jobs {
        assert_eq!(
            geometry.audit_admitted(job).await.unwrap().status,
            "SUCCEEDED"
        );
    }
    geometry.shutdown().await.unwrap();
    let assets = f
        .workspace
        .list_assets(f.session, f.dataset, None, 10)
        .unwrap()
        .items;
    let invalid = assets
        .iter()
        .find(|asset| asset.filename == "signal.png")
        .unwrap()
        .asset_id;
    let snapshot = f.store.get(f.session, run).unwrap();
    for task in &snapshot.geometry_tasks {
        assert_eq!(task.status, "SUCCEEDED");
        let report = read_geometry_report(&f.workspace, f.session, task.task_id).unwrap();
        let expected = if task.asset_id == invalid {
            "bbox_out_of_bounds"
        } else {
            "annotation_missing"
        };
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == expected)
        );
    }
    assert_eq!(fs::read(annotation).unwrap(), COCO);
    f.workspace.close(f.session).unwrap();
    let grant = f.workspace.grant_selected_directory(f.root).unwrap();
    let reopened = f
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    let recovered = f.store.get(reopened, run).unwrap();
    assert_eq!(recovered.status, WorkflowStatus::Interrupted);
    assert_eq!(recovered.geometry_tasks, snapshot.geometry_tasks);
    f.workspace.close(reopened).unwrap();
}
