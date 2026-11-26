use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::review_store::ReviewSession;
use dd_infra::workspace_thread::WorkspaceThread;
use uuid::Uuid;

pub const IMAGE: &[u8] = include_bytes!("../../../../testdata/image_io/p00-worker-rgb.png");
pub const ORIGINAL: &[u8] = br#"{"images":[{"id":1,"file_name":"sample.png","width":2,"height":1}],"annotations":[{"id":7,"image_id":1,"category_id":2,"bbox":[0,0,3,1],"area":3,"iscrowd":0}],"categories":[{"id":2,"name":"helmet"}]}"#;

pub struct ReviewFixture {
    pub root: PathBuf,
    pub formal: PathBuf,
    pub owner: Arc<WorkspaceThread>,
    pub session: Uuid,
    pub task: Uuid,
    pub opened: ReviewSession,
}

impl ReviewFixture {
    pub async fn open() -> Self {
        let runs = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
            .canonicalize()
            .unwrap();
        let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/contract-check/worker-rpc")
            .canonicalize()
            .unwrap();
        assert_eq!(runs.parent(), Some(allowed.as_path()));
        let run = runs.join(format!("review-{}", Uuid::new_v4()));
        let root = run.join("Workspace");
        let source = run.join("Inputs");
        fs::create_dir(&run).unwrap();
        fs::create_dir(&root).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("sample.png"), IMAGE).unwrap();
        let formal = source.join("sample.json");
        fs::write(&formal, ORIGINAL).unwrap();
        let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
        let selection = owner.grant_selected_directory(root.clone()).unwrap();
        let session = owner
            .create(selection.grant_id, "Review".into(), 10)
            .unwrap()
            .workspace_session_id;
        let importer = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
        let grant = owner.grant_import_directory(session, source).unwrap();
        let plan = importer
            .inspect(
                session,
                grant.grant_id,
                "Samples".into(),
                vec!["sample.png".into()],
            )
            .unwrap();
        let registered = importer
            .confirm(session, plan.plan_id, plan.fingerprint, 11)
            .unwrap();
        let dataset = Uuid::parse_str(&registered.dataset_id).unwrap();
        let asset = owner.list_assets(session, dataset, None, 1).unwrap().items[0].asset_id;
        let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
        let entrypoint = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../services/python-worker/src/dd_worker/entrypoint.py")
            .canonicalize()
            .unwrap();
        let mut worker = GeometryRuntime::launch(Arc::clone(&owner), session, python, entrypoint)
            .await
            .unwrap();
        let checked = worker
            .audit(GeometryAuditRequest {
                dataset_id: dataset,
                asset_id: asset,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                max_pixels: 100,
            })
            .await
            .unwrap();
        assert_eq!(checked.status, "SUCCEEDED");
        worker.shutdown().await.unwrap();
        let opened = owner.open_review(session, dataset, asset, 12).unwrap();
        Self {
            root,
            formal,
            owner,
            session,
            task: checked.task_id,
            opened,
        }
    }
}
