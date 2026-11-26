use std::path::PathBuf;

use dd_ui_contracts::workspace::WorkspaceCreateRequest;

use super::*;

#[test]
fn command_creates_from_a_trusted_grant_and_guides_opening_an_existing_workspace() {
    let root = PathBuf::from(
        std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR")
            .expect("set the desktop command test directory"),
    )
    .canonicalize()
    .unwrap();
    let runs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/desktop-commands/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(root.parent(), Some(runs.as_path()));
    std::fs::create_dir(root.join("workspace")).unwrap();
    let root = root.join("workspace");
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let request_id = UuidText::try_from(Uuid::new_v4().to_string()).unwrap();
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: request_id.clone(),
    };

    let create_request = |grant_id: Uuid| WorkspaceCreateRequest {
        request_id: request_id.clone(),
        directory_grant_id: UuidText::try_from(grant_id.to_string()).unwrap(),
        display_name: "命令验收".into(),
    };
    let Err(failure) = commands.create(create_request(Uuid::new_v4())) else {
        panic!("accepted an unknown grant");
    };
    assert_eq!(failure.code, "PERMISSION_DENIED");
    assert_eq!(failure.request_id, request_id);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

    let grant = thread.grant_selected_directory(root.clone()).unwrap();
    let WorkspaceSnapshot::Open {
        display_name,
        workspace_id,
        workspace_session_id,
        ..
    } = commands.create(create_request(grant.grant_id)).unwrap()
    else {
        panic!("workspace was not opened after creation");
    };
    assert_eq!(display_name, "命令验收");
    assert_eq!(
        String::from(workspace_id.clone()),
        thread.current().unwrap().unwrap().workspace_id
    );
    thread
        .close(commands.parse_uuid(workspace_session_id).unwrap())
        .unwrap();

    let grant = thread.grant_selected_directory(root).unwrap();
    let Err(failure) = commands.create(create_request(grant.grant_id)) else {
        panic!("accepted creation over an existing Workspace");
    };
    assert_eq!(failure.code, "FAILED_PRECONDITION");
    assert!(failure.message.contains("打开工作区"));
    assert!(thread.current().unwrap().is_none());
}
