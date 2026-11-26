use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime, read_review_image};
use dd_infra::json::{JsonContracts, JsonProfile};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const LABELS: &[u8] = br#"{"images":[{"id":1,"file_name":"registered.png","width":2,"height":1}],"annotations":[{"id":7,"image_id":1,"category_id":2,"bbox":[0,0,3,1],"area":3,"iscrowd":0}],"categories":[{"id":2,"name":"helmet"}]}"#;

#[tokio::test]
async fn imported_workspace_sample_reaches_the_actual_worker_and_durable_task_result() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/contract-check/worker-rpc")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let root = run.join("Registered Workspace");
    let source = run.join("Original dataset");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("registered.png"), PNG).unwrap();
    fs::write(source.join("registered.json"), LABELS).unwrap();
    fs::write(source.join("other.png"), PNG).unwrap();
    let workspace = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let root_grant = workspace.grant_selected_directory(root.clone()).unwrap();
    let opened = workspace
        .create(root_grant.grant_id, "Registered Workspace".into(), 10)
        .unwrap();
    let session = opened.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&workspace)).unwrap();
    let grant = workspace
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect(
            session,
            grant.grant_id,
            "Imported sample".into(),
            vec!["registered.png".into(), "other.png".into()],
        )
        .unwrap();
    let imported = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 20)
        .unwrap();
    let dataset_id = Uuid::parse_str(&imported.dataset_id).unwrap();
    let page = workspace.list_assets(session, dataset_id, None, 2).unwrap();
    let find_asset = |name: &str| {
        page.items
            .iter()
            .find(|asset| asset.filename == name)
            .unwrap()
            .asset_id
    };
    let asset_id = find_asset("registered.png");

    let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entrypoint = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/python-worker/src/dd_worker/entrypoint.py")
        .canonicalize()
        .unwrap();
    let mut geometry =
        GeometryRuntime::launch(Arc::clone(&workspace), session, interpreter, entrypoint)
            .await
            .unwrap();
    let request = GeometryAuditRequest {
        dataset_id,
        asset_id,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        max_pixels: 100,
    };
    let accepted = geometry.audit(request.clone()).await.unwrap();
    assert_eq!(accepted.status, "SUCCEEDED");
    assert_eq!(geometry.audit(request).await.unwrap(), accepted);
    let manifest = root.join(accepted.output_manifest_path.clone().unwrap());
    let report: Value =
        serde_json::from_slice(&fs::read(manifest.with_file_name("geometry-result.json")).unwrap())
            .unwrap();
    assert_eq!(report["findings"][0]["code"], "bbox_out_of_bounds");
    assert_eq!(report["findings"][0]["annotation_id"], 7);
    assert_eq!(fs::read(source.join("registered.png")).unwrap(), PNG);
    assert_eq!(fs::read(source.join("registered.json")).unwrap(), LABELS);

    let review = workspace
        .open_review(session, dataset_id, asset_id, 30)
        .unwrap();
    let image = read_review_image(&workspace, session, review.review_id, accepted.task_id).unwrap();
    assert_eq!(image.file_name, "registered.png");
    assert_eq!((image.width, image.height), (2, 1));
    assert_eq!(image.sha256, format!("{:x}", Sha256::digest(PNG)));
    assert!(read_review_image(&workspace, session, review.review_id, Uuid::new_v4()).is_err());
    assert!(
        read_review_image(
            &workspace,
            Uuid::new_v4(),
            review.review_id,
            accepted.task_id
        )
        .is_err()
    );
    let other = workspace
        .open_review(session, dataset_id, find_asset("other.png"), 31)
        .unwrap();
    assert!(read_review_image(&workspace, session, other.review_id, accepted.task_id).is_err());
    let reported_path = manifest.with_file_name("geometry-result.json");
    let honest_report = fs::read(&reported_path).unwrap();
    let mut edited = report;
    edited["image"]["width"] = Value::from(9);
    fs::write(&reported_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert!(read_review_image(&workspace, session, review.review_id, accepted.task_id).is_err());
    fs::write(&reported_path, honest_report).unwrap();

    geometry.shutdown().await.unwrap();
    workspace.close(session).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let (registered_dataset, input_path, fingerprint, revision): (String, String, String, i64) = db.query_row(
        "SELECT dataset_id, input_manifest_path, input_fingerprint, row_revision FROM tasks WHERE task_id=?1 AND status='SUCCEEDED'",
        [accepted.task_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(registered_dataset, dataset_id.to_string());
    assert_eq!(revision, 4);
    let contracts = JsonContracts::new().unwrap();
    let document = contracts
        .validate(
            JsonProfile::InputManifest,
            &fs::read(root.join(input_path)).unwrap(),
        )
        .unwrap();
    assert_eq!(document["dataset_id"], dataset_id.to_string());
    assert_eq!(document["assets"][0]["asset_id"], asset_id.to_string());
    assert_eq!(document["input_fingerprint"], fingerprint);
    assert_eq!(
        db.query_row("SELECT count(*) FROM tasks", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
