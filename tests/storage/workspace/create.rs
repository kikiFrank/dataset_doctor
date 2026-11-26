mod support;

use std::fs;
use std::path::Path;

use dd_infra::json::JsonContracts;
use dd_infra::sqlite::migrate_workspace_database;
use dd_infra::workspace::{WorkspaceLock, WorkspaceLockError, create_workspace, open_workspace};
use rusqlite::Connection;

const PENDING: &str = "workspace.json.pending";
const FIRST: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const SECOND: &str = "76afdb2d-0900-4528-b303-0fca0f864235";
const NAME: &str = "新建 合成工作区";

fn start(root: &Path, contracts: &JsonContracts) -> dd_infra::workspace::OpenedWorkspace {
    create_workspace(root, contracts, 250, NAME, FIRST, 1234).unwrap()
}

fn pending(root: &Path) {
    fs::write(
        root.join(PENDING),
        serde_json::to_vec(&serde_json::json!({
            "workspace_id": FIRST, "name": NAME,
            "config_format_version": 1, "config_revision": 0
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn create_publishes_a_valid_empty_workspace_and_opens_it_with_ownership() {
    let root = support::workspace_root("create 工作区");
    let contracts = JsonContracts::new().unwrap();
    let opened = start(&root, &contracts);
    assert_eq!(opened.identity().workspace_id, FIRST);
    assert_eq!(opened.identity().display_name, NAME);
    assert_eq!(opened.identity().config_revision, 0);
    assert_eq!(opened.identity().offline_asset_count, 0);
    assert!(matches!(
        WorkspaceLock::acquire(&root),
        Err(WorkspaceLockError::Busy)
    ));
    assert!(!root.join(PENDING).exists());
    drop(opened);

    let reopened = open_workspace(&root, &contracts, 250).unwrap();
    assert_eq!(reopened.identity().workspace_id, FIRST);
    drop(reopened);
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let created: i64 = db
        .query_row("SELECT created_at_ms FROM schema_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    let integrity: String = db
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .unwrap();
    let violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((created, integrity.as_str(), violations), (1234, "ok", 0));
}

#[test]
fn refuses_to_replace_existing_workspaces_or_claim_unrelated_files() {
    let contracts = JsonContracts::new().unwrap();
    let existing = support::workspace_root("create already initialized");
    drop(start(&existing, &contracts));
    let original = fs::read(existing.join("workspace.json")).unwrap();
    assert!(create_workspace(&existing, &contracts, 250, "New", SECOND, 9).is_err());
    assert_eq!(fs::read(existing.join("workspace.json")).unwrap(), original);
    assert_eq!(
        open_workspace(&existing, &contracts, 250)
            .unwrap()
            .identity()
            .workspace_id,
        FIRST
    );

    let unrelated = support::workspace_root("create unowned database");
    let database = unrelated.join("workspace.db");
    fs::write(&database, b"").unwrap();
    assert!(create_workspace(&unrelated, &contracts, 250, NAME, FIRST, 9).is_err());
    assert_eq!(fs::metadata(&database).unwrap().len(), 0);
    assert!(!unrelated.join(PENDING).exists());
    // An intent also cannot turn an independently introduced empty file into a database.
    pending(&unrelated);
    assert!(create_workspace(&unrelated, &contracts, 250, NAME, FIRST, 9).is_err());
    assert!(!unrelated.join("workspace.json").exists());
    assert_eq!(fs::metadata(&database).unwrap().len(), 0);
}

#[test]
fn resumes_a_saved_creation_before_and_after_database_publication() {
    let contracts = JsonContracts::new().unwrap();
    for database_published in [false, true] {
        let root = support::workspace_root(&format!("create interrupted {database_published}"));
        pending(&root);
        if database_published {
            let db = Connection::open(root.join("workspace.db")).unwrap();
            migrate_workspace_database(db, 250, FIRST, 1234)
                .unwrap()
                .close()
                .unwrap();
        }
        assert!(open_workspace(&root, &contracts, 250).is_err());
        assert!(create_workspace(&root, &contracts, 250, "Different", SECOND, 2345).is_err());
        assert_eq!(root.join("workspace.db").exists(), database_published);
        let reopened = create_workspace(&root, &contracts, 250, NAME, SECOND, 2345).unwrap();
        assert_eq!(reopened.identity().workspace_id, FIRST);
        assert!(!root.join(PENDING).exists());
    }
}

#[test]
fn rejects_invalid_marker_input_without_publishing_workspace_files() {
    let contracts = JsonContracts::new().unwrap();
    let root = support::workspace_root("create invalid input");
    assert!(create_workspace(&root, &contracts, 250, "", FIRST, 0).is_err());
    assert!(create_workspace(&root, &contracts, 250, NAME, "invalid-uuid", 0).is_err());
    for file in ["workspace.json", "workspace.db", PENDING] {
        assert!(!root.join(file).exists());
    }
}

#[test]
fn failed_database_initialization_can_retry_without_publishing_a_partial_database() {
    let contracts = JsonContracts::new().unwrap();
    let root = support::workspace_root("create initialization failure");
    let failed = create_workspace(&root, &contracts, u32::MAX, NAME, FIRST, 1234);
    assert!(failed.is_err());
    assert!(root.join(PENDING).is_file());
    assert!(!root.join("workspace.db").exists());
    assert!(!root.join("workspace.json").exists());
    assert!(WorkspaceLock::acquire(&root).is_ok());

    let created = create_workspace(&root, &contracts, 250, NAME, SECOND, 2345).unwrap();
    assert_eq!(created.identity().workspace_id, FIRST);
    assert!(!root.join(PENDING).exists());
}
