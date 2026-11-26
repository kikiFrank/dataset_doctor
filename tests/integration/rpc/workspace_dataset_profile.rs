use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::dataset_profile_runtime::{
    DatasetProfileRequest, DatasetProfileRuntime, read_dataset_profile,
};
use dd_infra::workspace_thread::WorkspaceThread;
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const COCO: &[u8] = br#"{"images":[{"id":1,"file_name":"positive.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,1,1],"area":1,"iscrowd":0}]}"#;

#[tokio::test]
async fn workspace_profiles_only_registered_assets_and_reopens_the_verified_result() {
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
    let root = run.join("Profile Workspace");
    let source = run.join("Profile sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    for name in ["positive.png", "unannotated.png", "unregistered.png"] {
        fs::write(source.join(name), PNG).unwrap();
    }
    fs::write(source.join("positive.json"), COCO).unwrap();
    let workspace = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = workspace.grant_selected_directory(root.clone()).unwrap();
    let session = workspace
        .create(grant.grant_id, "Profile".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&workspace)).unwrap();
    let grant = workspace
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect(
            session,
            grant.grant_id,
            "Selected images".into(),
            vec!["positive.png".into(), "unannotated.png".into()],
        )
        .unwrap();
    let receipt = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap();
    let dataset = receipt.dataset_id.parse().unwrap();
    let page = workspace.list_assets(session, dataset, None, 10).unwrap();
    let asset = |name| {
        page.items
            .iter()
            .find(|item| item.filename == name)
            .unwrap()
            .asset_id
    };
    let positive = asset("positive.png");
    let unannotated = asset("unannotated.png");

    let request = DatasetProfileRequest {
        dataset_id: dataset,
        asset_ids: vec![positive, unannotated],
        max_pixels: 100,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let mut invalid = request.clone();
    invalid.asset_ids = vec![positive, Uuid::new_v4()];
    assert!(DatasetProfileRuntime::admit(&workspace, session, invalid).is_err());
    let queued = DatasetProfileRuntime::admit(&workspace, session, request.clone()).unwrap();
    let task = queued.task().task_id;
    assert_eq!(queued.task().status, "QUEUED");
    let mut reversed = request.clone();
    reversed.asset_ids.reverse();
    assert_eq!(
        DatasetProfileRuntime::admit(&workspace, session, reversed)
            .unwrap()
            .task()
            .task_id,
        task
    );
    let mut conflicting = request.clone();
    conflicting.asset_ids.pop();
    assert!(DatasetProfileRuntime::admit(&workspace, session, conflicting).is_err());

    let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entry = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
    let mut worker = DatasetProfileRuntime::launch(
        Arc::clone(&workspace),
        session,
        python.clone(),
        entry.clone(),
    )
    .await
    .unwrap();
    fs::remove_file(source.join("unannotated.png")).unwrap();
    let outcome = worker.run(queued).await.unwrap();
    assert_eq!(outcome.status, "SUCCEEDED");
    assert_eq!(outcome.task_id, task);
    worker.shutdown().await.unwrap();
    let profile = read_dataset_profile(&workspace, session, dataset, task).unwrap();
    assert_eq!(profile.asset_count, 2);
    assert_eq!(profile.annotation_count, 1);
    assert_eq!(profile.class_counts.get("green-light"), Some(&1));
    assert_eq!(profile.unannotated_count, 1);
    assert_eq!(profile.file_errors.len(), 1);
    assert_eq!(profile.file_errors[0].asset_id, unannotated);
    assert_eq!(profile.file_errors[0].code, "offline");
    assert!(read_dataset_profile(&workspace, session, Uuid::new_v4(), task).is_err());
    assert_eq!(fs::read(source.join("positive.json")).unwrap(), COCO);

    let pending = DatasetProfileRuntime::admit(
        &workspace,
        session,
        DatasetProfileRequest {
            idempotency_key: Uuid::new_v4(),
            ..request
        },
    )
    .unwrap();
    let abandoned = pending.task().task_id;
    assert_eq!(pending.task().status, "QUEUED");
    workspace.close(session).unwrap();
    let grant = workspace.grant_selected_directory(root.clone()).unwrap();
    let reopened = workspace.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        read_dataset_profile(&workspace, reopened, dataset, task).unwrap(),
        profile
    );
    assert!(read_dataset_profile(&workspace, session, dataset, task).is_err());
    let mut worker = DatasetProfileRuntime::launch(Arc::clone(&workspace), reopened, python, entry)
        .await
        .unwrap();
    assert!(worker.run(pending).await.is_err());
    let recovered = workspace.task_outcome(reopened, abandoned).unwrap();
    assert_eq!(recovered.status, "INTERRUPTED");
    assert_eq!(recovered.error_code.as_deref(), Some("SESSION_ENDED"));
    worker.shutdown().await.unwrap();
    let manifest = root.join(outcome.output_manifest_path.unwrap());
    fs::write(
        manifest.with_file_name("dataset-profile-file-errors.json"),
        b"[]",
    )
    .unwrap();
    assert!(read_dataset_profile(&workspace, reopened, dataset, task).is_err());
    workspace.close(reopened).unwrap();
}
