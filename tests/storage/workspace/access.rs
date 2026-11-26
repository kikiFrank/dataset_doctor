mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dd_core::workspace::{WorkspaceAccess, WorkspaceAccessError};
use dd_infra::sqlite::migrate_workspace_database;
use dd_infra::workspace::{ExistingWorkspaceStore, WorkspaceLock, WorkspaceLockError};
use rusqlite::Connection;

const ID: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const TIME_TO_LIVE: Duration = Duration::from_secs(10);

fn seeded_workspace(root: &Path) {
    fs::write(
        root.join("workspace.json"),
        serde_json::to_vec(&serde_json::json!({
            "workspace_id": ID, "name": "授权 工作区",
            "config_format_version": 1, "config_revision": 0
        }))
        .unwrap(),
    )
    .unwrap();
    migrate_workspace_database(
        Connection::open(root.join("workspace.db")).unwrap(),
        250,
        ID,
        0,
    )
    .unwrap()
    .close()
    .unwrap();
}

#[test]
fn opens_an_authorized_real_workspace_and_rejects_the_old_session_after_reopen() {
    let root = support::workspace_root("access lifecycle");
    seeded_workspace(&root);
    let other = support::workspace_root("access another directory");
    seeded_workspace(&other);
    let mut core = WorkspaceAccess::new(ExistingWorkspaceStore::new(250).unwrap(), TIME_TO_LIVE);
    let now = Instant::now();
    let grant = core.grant_selected_directory(root.clone(), now).unwrap();
    let opened = core.open(grant.grant_id, now).unwrap();
    assert_eq!(opened.workspace_id, ID);
    assert_eq!(opened.display_name, "授权 工作区");
    assert!(matches!(
        WorkspaceLock::acquire(&root),
        Err(WorkspaceLockError::Busy)
    ));
    let other_grant = core.grant_selected_directory(other.clone(), now).unwrap();
    assert!(matches!(
        core.open(other_grant.grant_id, now),
        Err(WorkspaceAccessError::AlreadyOpen)
    ));
    assert!(!other.join(".datasetdoctor.lock").exists());
    core.close(opened.workspace_session_id).unwrap();
    assert!(WorkspaceLock::acquire(&root).is_ok());

    assert!(matches!(
        core.open(grant.grant_id, now),
        Err(WorkspaceAccessError::NotAuthorized)
    ));
    let grant = core.grant_selected_directory(root, now).unwrap();
    let reopened = core.open(grant.grant_id, now).unwrap();
    assert_ne!(reopened.workspace_session_id, opened.workspace_session_id);
    assert!(matches!(
        core.close(opened.workspace_session_id),
        Err(WorkspaceAccessError::StaleSession)
    ));
    assert_eq!(core.current().unwrap(), &reopened);
    core.close(reopened.workspace_session_id).unwrap();
    assert!(core.current().is_none());
}

#[test]
fn replaced_or_expired_grants_cannot_touch_workspace_files() {
    let root = support::workspace_root("access expired");
    seeded_workspace(&root);
    let mut core = WorkspaceAccess::new(ExistingWorkspaceStore::new(250).unwrap(), TIME_TO_LIVE);
    let now = Instant::now();
    assert!(matches!(
        core.grant_selected_directory(PathBuf::from("relative"), now),
        Err(WorkspaceAccessError::InvalidSelection)
    ));
    let old = core.grant_selected_directory(root.clone(), now).unwrap();
    let current = core.grant_selected_directory(root.clone(), now).unwrap();
    assert_ne!(old.grant_id, current.grant_id);
    assert!(matches!(
        core.open(old.grant_id, now),
        Err(WorkspaceAccessError::NotAuthorized)
    ));
    assert!(matches!(
        core.open(current.grant_id, now + TIME_TO_LIVE),
        Err(WorkspaceAccessError::ExpiredGrant)
    ));
    assert!(!root.join(".datasetdoctor.lock").exists());
}
