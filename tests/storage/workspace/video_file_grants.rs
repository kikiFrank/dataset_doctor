use std::fs;
use std::path::Path;
use std::time::Duration;

use dd_core::workspace::WorkspaceAccessError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

fn open(thread: &WorkspaceThread, root: &Path, create: bool) -> uuid::Uuid {
    let grant = thread.grant_selected_directory(root.to_owned()).unwrap();
    if create {
        thread.create(grant.grant_id, "Synthetic video".into(), 100)
    } else {
        thread.open(grant.grant_id)
    }
    .unwrap()
    .workspace_session_id
}

#[test]
fn video_selection_authorizes_exactly_one_regular_file_for_one_workspace_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let path = root.path().join("sample.mp4");
    let adjacent = root.path().join("adjacent.mp4");
    fs::write(&path, b"chosen synthetic media").unwrap();
    fs::write(&adjacent, b"unselected synthetic media").unwrap();
    let thread = WorkspaceThread::start(250, Duration::from_secs(4)).unwrap();
    let session = open(&thread, &workspace, true);
    assert!(matches!(
        thread.grant_video_file(session, root.path().to_owned()),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::InvalidSelection
        ))
    ));
    assert!(matches!(
        thread.grant_video_file(session, root.path().join("missing.mp4")),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::InvalidSelection
        ))
    ));
    let directory = thread
        .grant_import_directory(session, root.path().to_owned())
        .unwrap();
    assert!(matches!(
        thread.consume_video_file(session, directory.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
    let grant = thread.grant_video_file(session, path.to_owned()).unwrap();
    assert!(matches!(
        thread.consume_import_directory(session, grant.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
    let selected = thread.consume_video_file(session, grant.grant_id).unwrap();
    assert_eq!(selected, path.canonicalize().unwrap());
    assert_ne!(selected, adjacent.canonicalize().unwrap());
    assert!(matches!(
        thread.consume_video_file(session, grant.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
    let old = thread.grant_video_file(session, path.to_owned()).unwrap();
    thread.close(session).unwrap();
    let reopened = open(&thread, &workspace, false);
    assert!(matches!(
        thread.consume_video_file(session, old.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::StaleSession
        ))
    ));
    assert!(matches!(
        thread.consume_video_file(reopened, old.grant_id),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::NotAuthorized
        ))
    ));
}
