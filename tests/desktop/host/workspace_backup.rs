use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::dataset_import::{ImportOperationKind, TaskGetRequest};

use super::*;

#[test]
fn desktop_submits_an_authorized_backup_and_recalls_the_verified_receipt_after_workspace_reopen() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let root = run.join("Desktop backup Workspace");
    let destination = run.join("Desktop backup destination");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&destination).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let selection = thread.grant_selected_directory(root.clone()).unwrap();
    let workspace = thread
        .create(selection.grant_id, "Desktop backup".into(), 100)
        .unwrap();
    let uuid = |value: String| UuidText::try_from(value).unwrap();
    let context = |session: Uuid| CommandContext {
        request_id: uuid(Uuid::new_v4().to_string()),
        workspace_id: uuid(workspace.workspace_id.clone()),
        workspace_session_id: uuid(session.to_string()),
        idempotency_key: None,
    };
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: uuid(Uuid::new_v4().to_string()),
    };
    let session = workspace.workspace_session_id;
    assert!(commands.latest_backup(context(session)).unwrap().is_none());
    let grant = commands
        .backup_directory(context(session), destination.clone())
        .unwrap();
    assert_eq!(grant.allowed_operations, ["WORKSPACE_BACKUP"]);
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    let mut admission = context(session);
    admission.idempotency_key = Some(uuid(Uuid::new_v4().to_string()));
    let operation = commands
        .submit_backup(admission, grant.grant_id, true)
        .unwrap();
    assert!(matches!(
        operation.operation_kind,
        ImportOperationKind::WorkspaceBackup
    ));
    let imports = DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap();
    let unknown = commands
        .import_task(
            &imports,
            TaskGetRequest {
                context: context(session),
                task_id: uuid(Uuid::new_v4().to_string()),
            },
        )
        .unwrap_err();
    assert_eq!(unknown.code, "NOT_FOUND");
    let deadline = Instant::now() + Duration::from_secs(20);
    let finished = loop {
        let record = commands
            .import_task(
                &imports,
                TaskGetRequest {
                    context: context(session),
                    task_id: operation.task_id.clone(),
                },
            )
            .unwrap();
        assert!(matches!(
            record.operation_kind,
            ImportOperationKind::WorkspaceBackup
        ));
        if !matches!(record.status.as_str(), "QUEUED" | "RUNNING") {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "last backup state was {}",
            record.status
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(finished.status, "SUCCEEDED");
    let receipt = finished.workspace_backup.unwrap();
    let package = PathBuf::from(&receipt.package_path);
    assert_eq!(package.parent(), Some(destination.as_path()));
    assert!(package.join("backup-manifest.json").is_file());
    thread.close(session).unwrap();

    let selection = thread.grant_selected_directory(root).unwrap();
    let opened = thread.open(selection.grant_id).unwrap();
    let last = commands
        .latest_backup(context(opened.workspace_session_id))
        .unwrap()
        .unwrap();
    assert_eq!(String::from(last.task_id), String::from(operation.task_id));
    assert_eq!(last.status, "SUCCEEDED");
    assert_eq!(
        last.workspace_backup.unwrap().package_path,
        receipt.package_path
    );
    assert_eq!(fs::read_dir(destination).unwrap().count(), 1);
}
