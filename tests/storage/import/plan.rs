#[path = "../workspace/support.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dd_core::import_plan::ImportApprovalError;
use dd_infra::dataset_import::{DatasetImportCoordinator, DatasetImportError};
use dd_infra::import_registration::ImportRegistrationError;
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::{Connection, OpenFlags};

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[test]
fn confirmation_freezes_the_preview_and_registers_original_files_once() {
    let workspace = support::workspace_root("import plan Workspace");
    let source = support::workspace_root("import plan sources");
    let image = source.join("item.png");
    fs::write(&image, IMAGE).unwrap();
    let actor = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let directory = actor.grant_selected_directory(workspace.clone()).unwrap();
    let active = actor
        .create(directory.grant_id, "Import".into(), 0)
        .unwrap();
    let session = active.workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&actor)).unwrap();
    let inspect = |name: &str| {
        let grant = actor
            .grant_import_directory(session, source.clone())
            .unwrap();
        imports
            .inspect(
                session,
                grant.grant_id,
                name.into(),
                vec!["item.png".into()],
            )
            .unwrap()
    };
    let initial = inspect("Initial");
    assert_eq!(initial.preview.inputs.len(), 1);
    assert_eq!(datasets(&workspace), 0);
    assert!(matches!(
        imports.confirm(session, initial.plan_id, [0; 32], 100),
        Err(DatasetImportError::Approval(
            ImportApprovalError::FingerprintMismatch
        ))
    ));

    fs::write(&image, [IMAGE, b"changed bytes"].concat()).unwrap();
    assert!(matches!(
        imports.confirm(session, initial.plan_id, initial.fingerprint, 100),
        Err(DatasetImportError::Registration(
            ImportRegistrationError::InputChanged
        ))
    ));
    assert_eq!(datasets(&workspace), 0);
    fs::write(&image, IMAGE).unwrap();
    assert!(matches!(
        imports.confirm(session, initial.plan_id, initial.fingerprint, 101),
        Err(DatasetImportError::Approval(
            ImportApprovalError::UnknownPlan
        ))
    ));

    let confirmed = inspect("Confirmed");
    let (left, right) = std::thread::scope(|scope| {
        let confirm = || imports.confirm(session, confirmed.plan_id, confirmed.fingerprint, 200);
        let first = scope.spawn(confirm);
        let second = scope.spawn(confirm);
        (first.join().unwrap(), second.join().unwrap())
    });
    let (receipt, other) = if left.is_ok() {
        (left.unwrap(), right)
    } else {
        (right.unwrap(), left)
    };
    match other {
        Ok(replay) => assert_eq!(replay, receipt),
        Err(DatasetImportError::Approval(ImportApprovalError::Busy)) => {}
        Err(error) => panic!("unexpected concurrent confirmation: {error}"),
    }
    assert_eq!(
        imports
            .confirm(session, confirmed.plan_id, confirmed.fingerprint, 201)
            .unwrap(),
        receipt
    );
    assert_eq!(datasets(&workspace), 1);
    assert_eq!(receipt.asset_count, 1);
    assert_eq!(fs::read(&image).unwrap(), IMAGE);
    assert!(!source.join("item.json").exists());

    actor.close(session).unwrap();
    let directory = actor.grant_selected_directory(workspace.clone()).unwrap();
    let reopened = actor.open(directory.grant_id).unwrap();
    assert!(matches!(
        imports.confirm(
            reopened.workspace_session_id,
            confirmed.plan_id,
            confirmed.fingerprint,
            300
        ),
        Err(DatasetImportError::Approval(
            ImportApprovalError::UnknownPlan
        ))
    ));
    assert_eq!(datasets(&workspace), 1);
}

fn datasets(workspace: &Path) -> i64 {
    Connection::open_with_flags(
        workspace.join("workspace.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row("SELECT count(*) FROM datasets", [], |row| row.get(0))
    .unwrap()
}
