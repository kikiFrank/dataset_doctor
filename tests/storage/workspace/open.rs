mod support;

use std::fs;
use std::path::{Path, PathBuf};

use dd_infra::json::JsonContracts;
use dd_infra::sqlite::{WorkspaceMigrationError, migrate_workspace_database};
use dd_infra::workspace::{WorkspaceLock, WorkspaceLockError, WorkspaceOpenError, open_workspace};
use rusqlite::Connection;

const OWNER: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const OTHER: &str = "76afdb2d-0900-4528-b303-0fca0f864235";

fn fixture(name: &str, marker_owner: &str) -> PathBuf {
    let root = support::workspace_root(name);
    fs::write(
        root.join("workspace.json"),
        serde_json::to_vec(&serde_json::json!({
            "workspace_id": marker_owner, "name": "合成 工作区",
            "config_format_version": 1, "config_revision": 0
        }))
        .unwrap(),
    )
    .unwrap();
    root
}

fn seed_database(root: &Path) {
    let connection = Connection::open(root.join("workspace.db")).unwrap();
    migrate_workspace_database(connection, 250, OWNER, 0)
        .unwrap()
        .close()
        .unwrap();
}

#[test]
fn opening_a_valid_workspace_keeps_ownership_until_close() {
    let root = fixture("open valid 工作区", OWNER);
    seed_database(&root);
    let contracts = JsonContracts::new().unwrap();
    let opened = open_workspace(&root, &contracts, 250).unwrap();
    assert_eq!(opened.root(), root);
    assert_eq!(opened.identity().workspace_id, OWNER);
    assert_eq!(opened.identity().display_name, "合成 工作区");
    assert!(matches!(
        WorkspaceLock::acquire(&root),
        Err(WorkspaceLockError::Busy)
    ));
    drop(opened);
    assert!(WorkspaceLock::acquire(&root).is_ok());
}

#[test]
fn mismatched_or_future_database_metadata_is_rejected() {
    let root = fixture("open identity mismatch", OTHER);
    seed_database(&root);
    let contracts = JsonContracts::new().unwrap();
    assert!(matches!(
        open_workspace(&root, &contracts, 250),
        Err(WorkspaceOpenError::Migration(
            WorkspaceMigrationError::WorkspaceIdMismatch
        ))
    ));
    let version = fixture("open future version", OWNER);
    seed_database(&version);
    let connection = Connection::open(version.join("workspace.db")).unwrap();
    connection
        .execute("UPDATE schema_meta SET schema_version=2", [])
        .unwrap();
    connection.close().unwrap();
    assert!(matches!(
        open_workspace(&version, &contracts, 250),
        Err(WorkspaceOpenError::Migration(
            WorkspaceMigrationError::UnsupportedVersion(2)
        ))
    ));
    assert!(WorkspaceLock::acquire(&version).is_ok());
}

#[test]
fn malformed_markers_and_missing_or_empty_databases_are_not_opened() {
    let contracts = JsonContracts::new().unwrap();
    let invalid = fixture("open malformed marker", OWNER);
    fs::write(invalid.join("workspace.json"), br#"{"workspace_id":"bad"}"#).unwrap();
    assert!(matches!(
        open_workspace(&invalid, &contracts, 250),
        Err(WorkspaceOpenError::Marker(_))
    ));

    let absent = fixture("open missing database", OWNER);
    assert!(open_workspace(&absent, &contracts, 250).is_err());
    assert!(!absent.join("workspace.db").exists());
    let empty = fixture("open empty database", OWNER);
    Connection::open(empty.join("workspace.db"))
        .unwrap()
        .close()
        .unwrap();
    assert!(matches!(
        open_workspace(&empty, &contracts, 250),
        Err(WorkspaceOpenError::Migration(
            WorkspaceMigrationError::MissingMetadata
        ))
    ));
    let connection = Connection::open(empty.join("workspace.db")).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM main.sqlite_schema", [], |row| {
            row.get(0)
        })
        .unwrap();
    let journal: String = connection
        .pragma_query_value(Some("main"), "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!((count, journal.as_str()), (0, "delete"));
}
