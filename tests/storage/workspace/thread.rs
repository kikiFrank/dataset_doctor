mod support;

use std::fs;
use std::time::Duration;

use dd_core::workspace::WorkspaceAccessError;
use dd_infra::sqlite::migrate_workspace_database;
use dd_infra::workspace::WorkspaceLock;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use rusqlite::Connection;

const ID: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";

#[test]
fn concurrent_open_requests_are_serialized_and_shutdown_releases_sqlite_and_lock() {
    let root = support::workspace_root("data thread workspace");
    let marker = serde_json::json!({
        "workspace_id": ID, "name": "线程验收", "config_format_version": 1, "config_revision": 0
    });
    fs::write(root.join("workspace.json"), marker.to_string()).unwrap();
    migrate_workspace_database(
        Connection::open(root.join("workspace.db")).unwrap(),
        250,
        ID,
        0,
    )
    .unwrap()
    .close()
    .unwrap();

    let actor = WorkspaceThread::start(250, Duration::from_secs(10)).unwrap();
    let grant = actor.grant_selected_directory(root.clone()).unwrap();
    let (left, right) = std::thread::scope(|scope| {
        let first = scope.spawn(|| actor.open(grant.grant_id));
        let second = scope.spawn(|| actor.open(grant.grant_id));
        (first.join().unwrap(), second.join().unwrap())
    });
    let (left, right) = if left.is_ok() {
        (left, right)
    } else {
        (right, left)
    };
    let opened = left.unwrap();
    assert!(matches!(
        right,
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::AlreadyOpen
        ))
    ));
    assert_eq!(actor.current().unwrap(), Some(opened));
    drop(actor);
    assert!(WorkspaceLock::acquire(&root).is_ok());
}
