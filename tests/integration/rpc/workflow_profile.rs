use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dd_core::workflow::{PlanPolicy, ScopeKind, Step, WorkflowInput};
use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::dataset_profile_runtime::{DatasetProfileRuntime, read_dataset_profile};
use dd_infra::input_fingerprint::freeze_input_manifest;
use dd_infra::json::JsonContracts;
use dd_infra::workflow_store::{
    WorkflowDraftRequest, WorkflowError, WorkflowProfileRequest, WorkflowStatus, WorkflowStore,
};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use serde_json::{Value, json};
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[tokio::test]
async fn workflow_start_links_one_real_profile_task_and_keeps_its_result_across_reopen() {
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
    let root = run.join("Workflow Profile Workspace");
    let source = run.join("Workflow Profile Sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    for name in ["first.png", "second.png"] {
        fs::write(source.join(name), PNG).unwrap();
    }
    let workspace = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = workspace.grant_selected_directory(root.clone()).unwrap();
    let session = workspace
        .create(grant.grant_id, "Workflow Profile".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&workspace)).unwrap();
    let grant = workspace
        .grant_import_directory(session, source.clone())
        .unwrap();
    let preview = imports
        .inspect_directory(session, grant.grant_id, "Profiling scope".into())
        .unwrap();
    let registration = imports
        .confirm(session, preview.plan_id, preview.fingerprint, 2)
        .unwrap();
    let dataset: Uuid = registration.dataset_id.parse().unwrap();
    let members = workspace
        .list_assets(session, dataset, None, 10)
        .unwrap()
        .items;
    assert_eq!(members.len(), 2);
    let selected = members.iter().map(|member| member.asset_id).min().unwrap();
    let inputs: Vec<_> = members
        .iter()
        .map(|member| {
            let image = workspace
                .registered_image(session, dataset, member.asset_id)
                .unwrap();
            json!({"asset_id": member.asset_id, "media_digest": image.image.sha256,
            "annotation_digest": null, "source_metadata_digest": null})
        })
        .collect();
    let dataset_info = workspace.list_datasets(session, None, 1).unwrap();
    let frozen = freeze_input_manifest(
        &JsonContracts::new().unwrap(),
        json!({
            "dataset_id": dataset, "label_schema_digest": dataset_info.items[0].schema_fingerprint,
            "assets": inputs, "resolved_config": {}, "capability_id": "workflow_plan_preview.v1",
            "implementation_version": "1", "model_identity": null,
        }),
    )
    .unwrap();
    let input_path = PathBuf::from("artifacts/task-inputs/profile-plan.json");
    fs::create_dir_all(root.join(input_path.parent().unwrap())).unwrap();
    fs::write(root.join(&input_path), frozen.bytes).unwrap();
    let store = WorkflowStore::new(Arc::clone(&workspace)).unwrap();
    let draft = store
        .admit(
            session,
            WorkflowDraftRequest {
                dataset_id: dataset,
                source: WorkflowInput {
                    kind: ScopeKind::Dataset,
                    manifest_id: Uuid::new_v4(),
                    fingerprint: frozen.input_fingerprint,
                },
                template_id: "industrial_dataset_healthcheck.v1".into(),
                input_path,
                input_manifest_sha256: frozen.sha256,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
            3,
        )
        .unwrap();
    let policy = PlanPolicy {
        available_steps: BTreeSet::from([Step::DatasetProfile, Step::AnnotationGeometry]),
        allowed_model_profiles: BTreeSet::new(),
        max_sample_limit: 10,
        max_adjustments: 0,
        max_model_calls: 0,
        max_output_tokens: 0,
    };
    let ready = store
        .accept_default(session, draft.run_id, draft.row_revision, &policy, 1, 4)
        .unwrap();
    let request = WorkflowProfileRequest {
        reference: ready.plan_ref.clone().unwrap(),
        expected_row_revision: ready.row_revision,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        max_pixels: 100,
    };
    let mut forged = request.clone();
    forged.reference.input_fingerprint[0] ^= 1;
    assert!(matches!(
        store.start_profile(session, &policy, forged, 5),
        Err(WorkflowError::RevisionConflict)
    ));
    let db = Connection::open(root.join("workspace.db")).unwrap();
    db.execute_batch("CREATE TRIGGER synthetic_workflow_launch_failure BEFORE UPDATE OF status ON workflow_runs
        WHEN NEW.status='RUNNING' BEGIN SELECT RAISE(ABORT, 'synthetic workflow launch fault'); END;").unwrap();
    assert!(
        store
            .start_profile(session, &policy, request.clone(), 6)
            .is_err()
    );
    assert_eq!(
        store.get(session, ready.run_id).unwrap().status,
        WorkflowStatus::Ready
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM tasks", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.execute_batch("DROP TRIGGER synthetic_workflow_launch_failure")
        .unwrap();

    let barrier = std::sync::Barrier::new(2);
    let start = || {
        barrier.wait();
        store
            .start_profile(session, &policy, request.clone(), 7)
            .unwrap()
    };
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(start);
        let second = scope.spawn(start);
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_ne!(first.job.is_some(), second.job.is_some());
    let (started, repeated) = if first.job.is_some() {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(started.workflow.status, WorkflowStatus::Running);
    assert_eq!(started.workflow.plan_ref.as_ref(), Some(&request.reference));
    assert_eq!(started.workflow.row_revision, ready.row_revision + 1);
    let profile_node = started.workflow.profile_task.unwrap();
    assert_eq!(profile_node.status, "QUEUED");
    let task = profile_node.task_id;
    assert_eq!(repeated.workflow.profile_task.unwrap().task_id, task);
    let replay = store
        .start_profile(session, &policy, request.clone(), 8)
        .unwrap();
    assert_eq!(replay.workflow.profile_task.unwrap().task_id, task);
    assert!(replay.job.is_none());
    let conflicting = WorkflowProfileRequest {
        max_pixels: 101,
        ..request
    };
    assert!(matches!(
        store.start_profile(session, &policy, conflicting, 8),
        Err(WorkflowError::IdempotencyConflict)
    ));
    let (link, input_path): (String, String) = db.query_row(
        "SELECT workflow_run_id,input_manifest_path FROM tasks WHERE task_id=?1 AND node_id='dataset_profile'",
        [task.to_string()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(link, ready.run_id.to_string());
    let worker_input: Value =
        serde_json::from_slice(&fs::read(root.join(input_path)).unwrap()).unwrap();
    assert_eq!(worker_input["assets"].as_array().unwrap().len(), 1);
    assert_eq!(worker_input["assets"][0]["asset_id"], selected.to_string());
    drop(db);

    let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entry = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
    let mut worker =
        DatasetProfileRuntime::launch(Arc::clone(&workspace), session, interpreter, entry)
            .await
            .unwrap();
    assert_eq!(
        worker.run(started.job.unwrap()).await.unwrap().status,
        "SUCCEEDED"
    );
    worker.shutdown().await.unwrap();
    assert_eq!(
        store
            .get(session, ready.run_id)
            .unwrap()
            .profile_task
            .unwrap()
            .status,
        "SUCCEEDED"
    );
    assert_eq!(
        read_dataset_profile(&workspace, session, dataset, task)
            .unwrap()
            .asset_count,
        1
    );

    workspace.close(session).unwrap();
    let grant = workspace.grant_selected_directory(root).unwrap();
    let reopened = workspace.open(grant.grant_id).unwrap().workspace_session_id;
    let snapshot = store.get(reopened, ready.run_id).unwrap();
    assert_eq!(snapshot.status, WorkflowStatus::Interrupted);
    assert_eq!(snapshot.profile_task.unwrap().task_id, task);
    assert_eq!(
        read_dataset_profile(&workspace, reopened, dataset, task)
            .unwrap()
            .asset_count,
        1
    );
    workspace.close(reopened).unwrap();
}
