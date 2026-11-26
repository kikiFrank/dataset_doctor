#[path = "../workspace/support.rs"]
mod support;

use std::time::{Duration, Instant};

use dd_core::workspace::{WorkspaceAccess, WorkspaceAccessError};
use dd_infra::workspace::SqliteWorkspaceStore;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use uuid::Uuid;

const LIFETIME: Duration = Duration::from_secs(5);

#[test]
fn import_grants_can_only_be_used_once_in_the_workspace_that_issued_them() {
    let workspace = support::workspace_root("import authorization Workspace");
    let source = support::workspace_root("authorized import source");
    let thread = WorkspaceThread::start(250, LIFETIME).unwrap();
    let directory = thread.grant_selected_directory(workspace.clone()).unwrap();
    let current = thread
        .create(directory.grant_id, "Import".into(), 0)
        .unwrap();
    let session = current.workspace_session_id;
    assert!(matches!(
        thread.consume_import_directory(session, directory.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));

    let approved = thread
        .grant_import_directory(session, source.clone())
        .unwrap();
    let retained = thread
        .grant_import_directory(session, source.clone())
        .unwrap();
    assert!(matches!(
        thread.consume_import_directory(Uuid::new_v4(), approved.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::StaleSession
        ))
    ));
    assert_eq!(
        thread
            .consume_import_directory(session, approved.grant_id)
            .unwrap(),
        source
    );
    assert!(matches!(
        thread.consume_import_directory(session, approved.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 0);

    thread.close(session).unwrap();
    assert!(matches!(
        thread.open(retained.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
    let directory = thread.grant_selected_directory(workspace).unwrap();
    let reopened = thread.open(directory.grant_id).unwrap();
    assert!(matches!(
        thread.consume_import_directory(reopened.workspace_session_id, retained.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
}

#[test]
fn an_expired_import_grant_returns_no_source_path() {
    let workspace = support::workspace_root("import grant expiry Workspace");
    let source = support::workspace_root("expired import source");
    let mut core = WorkspaceAccess::new(SqliteWorkspaceStore::new(250).unwrap(), LIFETIME);
    let now = Instant::now();
    let directory = core.grant_selected_directory(workspace, now).unwrap();
    let active = core.create(directory.grant_id, "Import", now, 0).unwrap();
    let session = active.workspace_session_id;
    let pending = core
        .grant_import_directory(session, source.clone(), now)
        .unwrap();
    assert!(matches!(
        core.consume_import_directory(session, pending.grant_id, now + LIFETIME),
        Err(WorkspaceAccessError::ExpiredGrant)
    ));
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 0);
}
