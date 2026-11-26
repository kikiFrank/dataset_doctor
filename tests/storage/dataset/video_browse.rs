#[path = "../workspace/support.rs"]
mod support;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use dd_infra::video_registration::{VideoImport, VideoRegistry};
use dd_infra::workspace_thread::WorkspaceThread;
use uuid::Uuid;

#[test]
fn workspace_video_pages_survive_reopen_and_reject_invalid_sessions_and_page_sizes() {
    let root = support::workspace_root("Video browse Workspace");
    let source = support::workspace_root("Video browse sources");
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let opened = owner
        .create(grant.grant_id, "Video browse".into(), 100)
        .unwrap();
    let session = opened.workspace_session_id;
    let registry = VideoRegistry::new(Arc::clone(&owner)).unwrap();
    assert!(
        owner
            .list_videos(session, None, 1)
            .unwrap()
            .items
            .is_empty()
    );
    let mut expected = Vec::new();
    for name in ["one.mp4", "two.mp4", "three.mp4"] {
        let path = source.join(name);
        fs::write(&path, b"synthetic video registration for browsing").unwrap();
        let grant = owner.grant_video_file(session, path).unwrap();
        expected.push(
            registry
                .admit(
                    session,
                    VideoImport {
                        request_id: Uuid::new_v4(),
                        idempotency_key: Uuid::new_v4(),
                        file_grant_id: grant.grant_id,
                        display_name: Some(format!("Selected {name}")),
                        stream_index: None,
                    },
                    200,
                )
                .unwrap()
                .video_id,
        );
    }
    expected.sort();
    let first = owner.list_videos(session, None, 2).unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());
    assert!(
        first
            .items
            .iter()
            .all(|item| item.kind == "VIDEO" && item.filename.starts_with("Selected "))
    );
    let last = owner.list_videos(session, first.next_cursor, 2).unwrap();
    assert_eq!(last.items.len(), 1);
    assert!(last.next_cursor.is_none());
    let actual: Vec<_> = first
        .items
        .iter()
        .chain(&last.items)
        .map(|item| item.asset_id)
        .collect();
    assert_eq!(actual, expected);
    for size in [0, 201] {
        assert!(owner.list_videos(session, None, size).is_err());
    }
    assert!(owner.list_videos(Uuid::new_v4(), None, 1).is_err());
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap();
    assert!(owner.list_videos(session, None, 1).is_err());
    let actual: Vec<_> = owner
        .list_videos(reopened.workspace_session_id, None, 200)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.asset_id)
        .collect();
    assert_eq!(actual, expected);
}
