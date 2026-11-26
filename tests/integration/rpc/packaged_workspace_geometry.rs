use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime, read_geometry_report};
use dd_infra::worker_process::WorkerProgram;
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const LABELS: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn packaged_worker_checks_registered_media_and_publishes_a_reopenable_receipt() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/contract-check/worker-rpc")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let product = PathBuf::from(std::env::var_os("DD_PACKAGED_WORKER_EXE").unwrap())
        .canonicalize()
        .unwrap();
    assert!(product.is_file());
    let root = run.join("Packaged Worker Workspace");
    let source = run.join("Packaged Worker Inputs");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("signal.png"), PNG).unwrap();
    fs::write(source.join("signal.json"), LABELS).unwrap();

    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Packaged Worker".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let input = imports
        .inspect_directory(session, grant.grant_id, "Packaged sample".into())
        .unwrap();
    let dataset = imports
        .confirm(session, input.plan_id, input.fingerprint, 2)
        .unwrap()
        .dataset_id
        .parse()
        .unwrap();
    let asset = owner.list_assets(session, dataset, None, 1).unwrap().items[0].asset_id;

    assert!(
        GeometryRuntime::launch_with_program(
            Arc::clone(&owner),
            session,
            WorkerProgram::Packaged {
                executable: run.join("missing-worker.exe")
            },
        )
        .await
        .is_err()
    );

    let mut worker = GeometryRuntime::launch_with_program(
        Arc::clone(&owner),
        session,
        WorkerProgram::Packaged {
            executable: product.clone(),
        },
    )
    .await
    .unwrap();
    let outcome = worker
        .audit(GeometryAuditRequest {
            asset_id: asset,
            dataset_id: dataset,
            max_pixels: 4,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, "SUCCEEDED");
    assert!(root.join(outcome.output_manifest_path.unwrap()).is_file());
    worker.shutdown().await.unwrap();
    let report = read_geometry_report(&owner, session, outcome.task_id).unwrap();
    assert_eq!((report.image.width, report.image.height), (2, 1));
    assert_eq!(report.findings[0].code, "bbox_out_of_bounds");
    assert_eq!(fs::read(source.join("signal.png")).unwrap(), PNG);
    assert_eq!(fs::read(source.join("signal.json")).unwrap(), LABELS);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(read_geometry_report(&owner, session, outcome.task_id).is_err());
    let historical = read_geometry_report(&owner, reopened, outcome.task_id).unwrap();
    assert_eq!(historical.findings[0].code, "bbox_out_of_bounds");
    owner.close(reopened).unwrap();
    let database = Connection::open(root.join("workspace.db")).unwrap();
    let integrity: String = database
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    println!(
        "Verified Workspace receipt from relocated packaged Worker at {}",
        product.display()
    );
}
