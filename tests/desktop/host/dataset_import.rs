use std::fs;
use std::path::PathBuf;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::dataset_import::{
    DatasetImportExternalRequest, DatasetImportInspectRequest, ImportFormat, TaskGetRequest,
};

use super::*;

#[test]
fn desktop_import_previews_and_confirms_through_queryable_background_tasks() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/desktop-commands/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let root = run.join("Desktop import Workspace");
    let input = run.join("Desktop import sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&input).unwrap();
    fs::write(
        input.join("image.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = thread.grant_selected_directory(root).unwrap();
    let opened = thread.create(grant.grant_id, "Import".into(), 1).unwrap();
    let session = opened.workspace_session_id;
    let imports = Arc::new(DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap());
    let id = |value: String| UuidText::try_from(value).unwrap();
    let context = |key: Option<UuidText>| CommandContext {
        request_id: id(Uuid::new_v4().to_string()),
        workspace_id: id(opened.workspace_id.clone()),
        workspace_session_id: id(session.to_string()),
        idempotency_key: key,
    };
    let commands = Commands {
        thread: Arc::clone(&thread),
        request_id: id(Uuid::new_v4().to_string()),
    };
    let grant = commands.import_directory(context(None), input).unwrap();
    let inspection = commands
        .start_import_inspection(
            Arc::clone(&imports),
            DatasetImportInspectRequest {
                context: context(Some(id(Uuid::new_v4().to_string()))),
                root_grant_id: grant.grant_id,
                dataset_name: "Imported".into(),
                format: ImportFormat::CocoImageBbox,
            },
        )
        .unwrap();
    let wait = |task_id: UuidText| {
        let deadline = std::time::Instant::now() + Duration::from_secs(12);
        loop {
            let snapshot = match commands.import_task(
                &imports,
                TaskGetRequest {
                    context: context(None),
                    task_id: task_id.clone(),
                },
            ) {
                Err(error) if error.retryable => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                result => result.unwrap(),
            };
            if !matches!(snapshot.status.as_str(), "QUEUED" | "RUNNING") {
                return snapshot;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let preview = wait(inspection.task_id).preview.unwrap();
    assert_eq!(preview.files[0].image_path, "image.png");
    assert!(preview.can_confirm);
    assert!(
        thread
            .list_datasets(session, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    let key = id(Uuid::new_v4().to_string());
    let record = || DatasetImportExternalRequest {
        context: context(Some(key.clone())),
        import_plan_ref: preview.plan_id.clone(),
        plan_fingerprint: preview.plan_fingerprint.clone(),
    };
    let operation = commands
        .start_import_confirmation(Arc::clone(&imports), record())
        .unwrap();
    let result = wait(operation.task_id.clone());
    assert_eq!(result.status, "SUCCEEDED");
    assert_eq!(String::from(result.registration.unwrap().asset_count), "1");
    assert_eq!(
        commands
            .start_import_confirmation(imports, record())
            .unwrap()
            .task_id,
        operation.task_id
    );
    assert_eq!(
        thread.list_datasets(session, None, 10).unwrap().items.len(),
        1
    );
    thread.close(session).unwrap();
}
