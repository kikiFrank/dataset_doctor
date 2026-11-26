use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dd_infra::import_preflight::inspect_single_image_coco;
use dd_infra::json::JsonContracts;
use dd_infra::workspace::create_workspace;
use dd_infra::workspace_backup::{
    WorkspacePackageError, create_workspace_backup, restore_workspace_backup,
};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::{Connection, OpenFlags, params};
use uuid::Uuid;

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const HISTORY_PATH: &str = "artifacts/task-inputs/backup-history.json";
const HISTORY: &[u8] = b"{\"test\":\"recorded task input\"}";

struct PackagedFixture {
    _root: tempfile::TempDir,
    container: PathBuf,
    source: PathBuf,
    package: PathBuf,
    image_in_package: PathBuf,
    workspace_id: Uuid,
    dataset_id: Uuid,
}

impl PackagedFixture {
    fn build() -> Self {
        let root = tempfile::tempdir().unwrap();
        let container = root.path().canonicalize().unwrap();
        let source = container.join("Original external images");
        let workspace = container.join("Original Workspace");
        let package = container.join("Backup package");
        for directory in [&source, &workspace] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(source.join("sample.png"), IMAGE).unwrap();
        fs::write(
            source.join("unregistered.env"),
            b"unregistered synthetic bytes",
        )
        .unwrap();
        let labels = serde_json::json!({
            "images": [{"id": 1, "file_name": "sample.png", "width": 2, "height": 1}],
            "annotations": [],
            "categories": [{"id": 1, "name": "test"}],
        });
        fs::write(source.join("sample.json"), labels.to_string()).unwrap();
        let contracts = JsonContracts::new().unwrap();
        let input = inspect_single_image_coco(&source, &contracts, &["sample.png".into()]).unwrap();
        let workspace_id = Uuid::new_v4();
        let mut opened = create_workspace(
            &workspace,
            &contracts,
            250,
            "Backup round trip",
            &workspace_id.to_string(),
            100,
        )
        .unwrap();
        let registration = opened
            .register_external_coco(&contracts, "Synthetic", &input, 200)
            .unwrap();
        let history = workspace.join(HISTORY_PATH);
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        fs::write(history, HISTORY).unwrap();
        let recorded = Connection::open(workspace.join("workspace.db")).unwrap();
        recorded.execute(
            "INSERT INTO tasks(task_id, workspace_session_id, capability_id, status, request_id,
                operation_kind, idempotency_key, request_fingerprint, input_manifest_path,
                input_fingerprint, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'fixture', 'SUCCEEDED', ?3, 'FIXTURE', ?4, ?5, ?6, ?7, 200, 200)",
            params![Uuid::new_v4().to_string(), Uuid::new_v4().to_string(),
                Uuid::new_v4().to_string(), Uuid::new_v4().to_string(), "1e".repeat(32),
                HISTORY_PATH, "2e".repeat(32)],
        ).unwrap();
        drop(recorded);
        let receipt = create_workspace_backup(&opened, &package).unwrap();
        assert_eq!(receipt.workspace_id, workspace_id);
        assert_eq!(receipt.asset_files, 2);
        assert!(!package.join("workspace.db-wal").exists());
        assert!(!package.join("workspace.db-shm").exists());
        assert_eq!(fs::read(package.join(HISTORY_PATH)).unwrap(), HISTORY);
        let asset = fs::read_dir(package.join("assets"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let image_in_package = asset.join("sample.png");
        assert_eq!(fs::read(&image_in_package).unwrap(), IMAGE);
        assert!(!asset.join("unregistered.env").exists());
        let db = read_only(&package.join("workspace.db"));
        let registered: u32 = db
            .query_row("SELECT count(*) FROM dataset_assets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(registered, 1);
        let duplicate = create_workspace_backup(&opened, &package).unwrap_err();
        assert!(matches!(
            duplicate,
            WorkspacePackageError::DestinationExists
        ));
        Self {
            _root: root,
            container,
            source,
            package,
            image_in_package,
            workspace_id,
            dataset_id: Uuid::parse_str(&registration.dataset_id).unwrap(),
        }
    }
}

#[test]
fn restore_reopens_a_new_workspace_and_reads_the_original_image_and_coco_json_without_external_files()
 {
    let fixture = PackagedFixture::build();
    let original_json = fs::read(fixture.source.join("sample.json")).unwrap();
    fs::remove_file(fixture.source.join("sample.png")).unwrap();
    fs::remove_file(fixture.source.join("sample.json")).unwrap();
    let restored = fixture.container.join("Restored Workspace");
    let receipt = restore_workspace_backup(&fixture.package, &restored).unwrap();
    assert_eq!(receipt.workspace_id, fixture.workspace_id);
    let thread = WorkspaceThread::start(250, Duration::from_secs(5)).unwrap();
    let selection = thread.grant_selected_directory(restored.clone()).unwrap();
    let session = thread.open(selection.grant_id).unwrap();
    assert_eq!(session.workspace_id, fixture.workspace_id.to_string());
    let listed = thread
        .list_assets(session.workspace_session_id, fixture.dataset_id, None, 10)
        .unwrap();
    let [asset] = listed.items.as_slice() else {
        panic!("restored image was not registered")
    };
    let binding = thread
        .registered_image(
            session.workspace_session_id,
            fixture.dataset_id,
            asset.asset_id,
        )
        .unwrap();
    assert!(binding.image.path.starts_with(&restored));
    assert_eq!(fs::read(binding.image.path).unwrap(), IMAGE);
    assert_eq!(
        fs::read(binding.annotation.unwrap().path).unwrap(),
        original_json
    );
    thread.close(session.workspace_session_id).unwrap();
    let db = read_only(&restored.join("workspace.db"));
    let report: String = db
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .unwrap();
    assert_eq!(report, "ok");
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(fs::read(fixture.image_in_package).unwrap(), IMAGE);
    assert_eq!(fs::read(restored.join(HISTORY_PATH)).unwrap(), HISTORY);
}

#[test]
fn restore_refuses_tampered_content_and_preexisting_targets_without_publishing_partial_data() {
    let fixture = PackagedFixture::build();
    let occupied = fixture.container.join("Existing data");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep.txt"), b"caller-owned fixture").unwrap();
    assert!(matches!(
        restore_workspace_backup(&fixture.package, &occupied),
        Err(WorkspacePackageError::DestinationExists)
    ));
    assert_eq!(
        fs::read(occupied.join("keep.txt")).unwrap(),
        b"caller-owned fixture"
    );
    fs::write(&fixture.image_in_package, b"changed package image").unwrap();
    let target = fixture.container.join("Broken restore");
    assert!(matches!(
        restore_workspace_backup(&fixture.package, &target),
        Err(WorkspacePackageError::DigestMismatch(_))
    ));
    assert!(!target.exists());
    assert_eq!(fs::read(fixture.source.join("sample.png")).unwrap(), IMAGE);
}

#[test]
fn a_backup_missing_its_recorded_artifact_inventory_cannot_be_restored() {
    let fixture = PackagedFixture::build();
    let manifest = fixture.package.join("backup-manifest.json");
    let mut body: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    body["artifacts"] = serde_json::json!([]);
    fs::write(manifest, serde_json::to_vec(&body).unwrap()).unwrap();
    let target = fixture.container.join("Incomplete restore");
    assert!(restore_workspace_backup(&fixture.package, &target).is_err());
    assert!(!target.exists());
    assert_eq!(
        fs::read(fixture.package.join(HISTORY_PATH)).unwrap(),
        HISTORY
    );
}

fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}
