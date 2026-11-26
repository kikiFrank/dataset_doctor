use std::fs;
use std::path::Path;

use dd_infra::sqlite::{
    WorkspaceBackupError, WorkspaceMigrationError, backup_workspace_database,
    migrate_workspace_database,
};
use rusqlite::{Connection, OpenFlags};

fn workspace(path: &Path) -> Connection {
    let source = migrate_workspace_database(
        Connection::open(path).unwrap(),
        250,
        "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9",
        1_794_000_000_000,
    )
    .unwrap();
    source
        .execute_batch(
            "PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE fixture_parent(id INTEGER PRIMARY KEY);
             CREATE TABLE fixture_child(
                 parent_id INTEGER NOT NULL REFERENCES fixture_parent(id)
             );
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO fixture_parent VALUES (7);
             INSERT INTO fixture_child VALUES (7);",
        )
        .unwrap();
    source
}

#[test]
fn standalone_snapshot_contains_committed_wal_and_reopens_without_sidecars() {
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join("workspace.db");
    let source = workspace(&source_path);
    let destination = root.path().join("snapshot.db");
    assert!(
        fs::metadata(source_path.with_file_name("workspace.db-wal"))
            .unwrap()
            .len()
            > 0
    );
    let loose_copy = root.path().join("loose-copy.db");
    fs::copy(&source_path, &loose_copy).unwrap();
    let raw = Connection::open(loose_copy).unwrap();
    assert_eq!(
        raw.query_row("SELECT count(*) FROM fixture_child", [], |row| row
            .get::<_, u32>(0))
            .unwrap(),
        0
    );

    backup_workspace_database(&source, &destination).unwrap();
    source
        .execute("INSERT INTO fixture_parent VALUES (8)", [])
        .unwrap();
    assert!(!root.path().join("snapshot.db-wal").exists());
    assert!(!root.path().join("snapshot.db-shm").exists());
    let snapshot =
        Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let persisted = snapshot
        .query_row("SELECT parent_id FROM fixture_child", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(persisted, 7);
    assert_eq!(
        snapshot
            .query_row("SELECT count(*) FROM fixture_parent", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot
            .pragma_query_value(None, "integrity_check", |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !snapshot
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}

#[test]
fn existing_destination_and_active_transaction_are_left_untouched() {
    let root = tempfile::tempdir().unwrap();
    let mut source = workspace(&root.path().join("workspace.db"));
    let existing = root.path().join("existing.db");
    fs::write(&existing, b"caller file").unwrap();
    assert!(matches!(
        backup_workspace_database(&source, &existing),
        Err(WorkspaceBackupError::DestinationExists)
    ));
    assert_eq!(fs::read(&existing).unwrap(), b"caller file");

    let new = root.path().join("backup.db");
    let tx = source.transaction().unwrap();
    tx.execute("DELETE FROM fixture_child", []).unwrap();
    assert!(matches!(
        backup_workspace_database(&tx, &new),
        Err(WorkspaceBackupError::ActiveTransaction)
    ));
    assert!(!new.exists());
    tx.rollback().unwrap();
    backup_workspace_database(&source, &new).unwrap();
}

#[test]
fn foreign_key_violation_discards_the_entire_unpublished_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(&root.path().join("workspace.db"));
    source.pragma_update(None, "foreign_keys", false).unwrap();
    source
        .execute("INSERT INTO fixture_child VALUES (123)", [])
        .unwrap();
    let output = root.path().join("output");
    fs::create_dir(&output).unwrap();
    assert!(matches!(
        backup_workspace_database(&source, &output.join("backup.db")),
        Err(WorkspaceBackupError::InvalidSnapshot(
            WorkspaceMigrationError::ForeignKeyCheck { .. }
        ))
    ));
    assert_eq!(fs::read_dir(output).unwrap().count(), 0);
}
