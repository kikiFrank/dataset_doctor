use std::fs;
use std::path::PathBuf;

use dd_infra::json::JsonContracts;
use dd_infra::workspace::create_workspace;
use dd_infra::workspace_backup::create_workspace_backup;

use super::*;

#[test]
fn desktop_restore_from_trusted_folders_opens_the_new_workspace_and_preserves_existing_paths() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/desktop-commands/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let original = run.join("Original backup fixture");
    fs::create_dir(&original).unwrap();
    let backup = run.join("Desktop restore package");
    let expected = Uuid::new_v4();
    let created = create_workspace(
        &original,
        &JsonContracts::new().unwrap(),
        250,
        "Backup source",
        &expected.to_string(),
        100,
    )
    .unwrap();
    create_workspace_backup(&created, &backup).unwrap();
    drop(created);
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: UuidText::try_from(Uuid::new_v4().to_string()).unwrap(),
    };
    let blocked = run.join("Already here");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("keep.txt"), b"test-owned existing file").unwrap();
    assert_eq!(
        commands
            .restore_backup(backup.clone(), run.clone(), "Already here".into())
            .err()
            .unwrap()
            .code,
        "CONFLICT"
    );
    assert_eq!(
        fs::read(blocked.join("keep.txt")).unwrap(),
        b"test-owned existing file"
    );
    for unsafe_name in ["../escape", "sub\\folder", "CON.backup", "trailing.", ""] {
        assert_eq!(
            commands
                .restore_backup(backup.clone(), run.clone(), unsafe_name.into())
                .err()
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
    }
    let invalid_package = run.join("Incomplete package");
    fs::create_dir(&invalid_package).unwrap();
    assert_eq!(
        commands
            .restore_backup(invalid_package, run.clone(), "Invalid Workspace".into())
            .err()
            .unwrap()
            .code,
        "FAILED_PRECONDITION"
    );
    assert!(!run.join("Invalid Workspace").exists());
    assert!(thread.current().unwrap().is_none());
    let restored = commands
        .restore_backup(backup.clone(), run.clone(), "Restored Workspace".into())
        .unwrap();
    assert!(matches!(restored, WorkspaceSnapshot::Open { .. }));
    let session = thread.current().unwrap().unwrap();
    assert_eq!(session.workspace_id, expected.to_string());
    assert!(run.join("Restored Workspace/workspace.db").is_file());
    assert!(original.join("workspace.db").is_file());
    assert_eq!(
        commands
            .restore_backup(backup, run.clone(), "Another restore".into())
            .err()
            .unwrap()
            .code,
        "FAILED_PRECONDITION"
    );
    assert!(!run.join("Another restore").exists());
}

#[test]
fn simultaneous_restore_commands_publish_only_the_workspace_the_owner_opens() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let original = run.join("Concurrent restore source");
    fs::create_dir(&original).unwrap();
    let package = run.join("Concurrent restore package");
    let created = create_workspace(
        &original,
        &JsonContracts::new().unwrap(),
        250,
        "Concurrent restore source",
        &Uuid::new_v4().to_string(),
        100,
    )
    .unwrap();
    create_workspace_backup(&created, &package).unwrap();
    drop(created);
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let ready = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let launch = |folder: &'static str| {
            let (thread, ready, package, run) = (&thread, &ready, &package, &run);
            scope.spawn(move || {
                let commands = Commands {
                    thread: Arc::clone(thread),
                    request_id: UuidText::try_from(Uuid::new_v4().to_string()).unwrap(),
                };
                ready.wait();
                commands.restore_backup(package.clone(), run.clone(), folder.into())
            })
        };
        let first = launch("First competing restore");
        let second = launch("Second competing restore");
        let mut open = 0;
        for result in [first.join().unwrap(), second.join().unwrap()] {
            match result {
                Ok(WorkspaceSnapshot::Open { .. }) => open += 1,
                Ok(WorkspaceSnapshot::Closed) => panic!("restore returned a closed Workspace"),
                Err(error) => assert_eq!(error.code, "FAILED_PRECONDITION"),
            }
        }
        assert_eq!(open, 1);
    });
    assert_ne!(
        run.join("First competing restore/workspace.db").is_file(),
        run.join("Second competing restore/workspace.db").is_file(),
    );
}
