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

#[test]
fn create_uses_a_one_shot_grant_and_opens_a_reusable_workspace() {
    let root = support::workspace_root("create via data thread");
    let actor = WorkspaceThread::start(250, Duration::from_secs(10)).unwrap();
    let grant = actor.grant_selected_directory(root.clone()).unwrap();
    let (left, right) = std::thread::scope(|scope| {
        let first = scope.spawn(|| actor.create(grant.grant_id, "创建验收".into(), 1000));
        let second = scope.spawn(|| actor.create(grant.grant_id, "创建验收".into(), 1000));
        (first.join().unwrap(), second.join().unwrap())
    });
    let (created, rejected) = if left.is_ok() {
        (left, right)
    } else {
        (right, left)
    };
    let created = created.unwrap();
    assert!(matches!(
        rejected,
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::AlreadyOpen
        ))
    ));
    assert_eq!(created.display_name, "创建验收");
    assert_eq!(created.offline_asset_count, 0);
    assert_eq!(actor.current().unwrap(), Some(created.clone()));

    actor.close(created.workspace_session_id).unwrap();
    assert!(matches!(
        actor.create(grant.grant_id, "Retry".into(), 2000),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
    let fresh = actor.grant_selected_directory(root).unwrap();
    let opened = actor.open(fresh.grant_id).unwrap();
    assert_eq!(opened.workspace_id, created.workspace_id);
    assert_ne!(opened.workspace_session_id, created.workspace_session_id);
}

#[test]
fn expired_creation_grant_leaves_the_directory_untouched() {
    let root = support::workspace_root("create expired grant");
    let actor = WorkspaceThread::start(250, Duration::ZERO).unwrap();
    let grant = actor.grant_selected_directory(root.clone()).unwrap();
    assert!(matches!(
        actor.create(grant.grant_id, "Denied".into(), 0),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::ExpiredGrant
        ))
    ));
    assert!(actor.current().unwrap().is_none());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}
