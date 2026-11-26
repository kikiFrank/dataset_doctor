#[path = "../workspace/support.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dd_core::import_plan::ImportApprovalError;
use dd_infra::dataset_import::{
    DatasetImportCoordinator, DatasetImportError, ImportConfirmation, ImportInspection,
};
use dd_infra::import_registration::ImportRegistrationError;
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::{Connection, OpenFlags};

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[test]
fn preflight_task_uses_one_grant_and_reports_a_read_only_preview_after_restart() {
    use uuid::Uuid;
    let root = support::workspace_root("import preflight task Workspace");
    let source = support::workspace_root("import preflight task inputs");
    let file = source.join("example.png");
    fs::write(&file, IMAGE).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Inspection".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner.grant_import_directory(session, source).unwrap();
    let request = ImportInspection {
        directory_grant_id: grant.grant_id,
        dataset_name: "Proposed dataset".into(),
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let queued = imports
        .submit_inspection(session, request.clone(), 2)
        .unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert!(queued.preview.is_none());
    let completed = imports.run_inspection(session, request.clone(), 3).unwrap();
    assert_eq!(completed.task_id, queued.task_id);
    assert_eq!(completed.status, "SUCCEEDED");
    let preview = completed.preview.as_ref().unwrap();
    assert_eq!(preview.files.len(), 1);
    assert_eq!(preview.files[0].image_path, "example.png");
    assert_eq!(preview.files[0].annotation_state, "MISSING");
    assert!(preview.can_confirm);
    assert_eq!(
        imports.run_inspection(session, request.clone(), 4).unwrap(),
        completed
    );
    let mut conflict = request;
    conflict.dataset_name = "Different".into();
    assert!(imports.submit_inspection(session, conflict, 5).is_err());
    assert!(
        owner
            .list_datasets(session, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(fs::read(&file).unwrap(), IMAGE);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let after = imports
        .inspection_task(reopened, queued.task_id, 6)
        .unwrap();
    assert_eq!(after.status, "SUCCEEDED");
    assert!(!after.preview.unwrap().can_confirm);
    assert!(
        owner
            .list_datasets(reopened, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    owner.close(reopened).unwrap();
}

#[test]
fn import_task_is_deduplicated_and_its_committed_receipt_survives_reopen() {
    use uuid::Uuid;
    let root = support::workspace_root("durable import Workspace");
    let source = support::workspace_root("durable import inputs");
    fs::write(source.join("test.png"), IMAGE).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Durable import".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Inputs".into())
        .unwrap();
    let mut request = ImportConfirmation {
        plan_id: plan.plan_id,
        plan_fingerprint: plan.fingerprint,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let queued = imports.submit_confirmation(session, request, 2).unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert!(queued.registration.is_none());
    assert!(
        owner
            .list_datasets(session, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    request.request_id = Uuid::new_v4();
    assert_eq!(
        imports.submit_confirmation(session, request, 3).unwrap(),
        queued
    );
    let mut other_key = request;
    other_key.idempotency_key = Uuid::new_v4();
    assert!(imports.submit_confirmation(session, other_key, 3).is_err());
    let completed = imports.run_confirmation(session, request, 4).unwrap();
    assert_eq!(completed.task_id, queued.task_id);
    assert_eq!(completed.status, "SUCCEEDED");
    assert_eq!(completed.registration.as_ref().unwrap().asset_count, 1);
    // Simulate losing the response and recreating the application.
    owner.close(session).unwrap();
    drop(imports);
    drop(owner);

    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    assert!(
        imports
            .confirmation_task(session, queued.task_id, 5)
            .is_err()
    );
    assert_eq!(
        imports
            .confirmation_task(reopened, queued.task_id, 5)
            .unwrap(),
        completed
    );
    request.request_id = Uuid::new_v4();
    assert_eq!(
        imports.run_confirmation(reopened, request, 6).unwrap(),
        completed
    );
    request.plan_fingerprint = [0; 32];
    assert!(imports.submit_confirmation(reopened, request, 7).is_err());
    assert_eq!(
        owner.list_datasets(reopened, None, 10).unwrap().items.len(),
        1
    );
    owner.close(reopened).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM tasks WHERE operation_kind='DATASET_IMPORT_EXTERNAL'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn failed_task_completion_rolls_back_dataset_and_asset_registration() {
    use uuid::Uuid;
    let root = support::workspace_root("rollback import Workspace");
    let source = support::workspace_root("rollback import inputs");
    fs::write(source.join("rollback.png"), IMAGE).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let opened = owner.create(grant.grant_id, "Rollback".into(), 1).unwrap();
    owner.close(opened.workspace_session_id).unwrap();
    {
        let db = Connection::open(root.join("workspace.db")).unwrap();
        db.execute_batch("CREATE TRIGGER reject_import_success BEFORE UPDATE OF status ON tasks
          WHEN NEW.status='SUCCEEDED' BEGIN SELECT RAISE(ABORT, 'synthetic final update failure'); END;").unwrap();
    }
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner.grant_import_directory(session, source).unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Rollback inputs".into())
        .unwrap();
    let request = ImportConfirmation {
        plan_id: plan.plan_id,
        plan_fingerprint: plan.fingerprint,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let failed = imports.run_confirmation(session, request, 2).unwrap();
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("IMPORT_FAILED"));
    assert!(failed.registration.is_none());
    assert!(
        owner
            .list_datasets(session, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    owner.close(session).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT dataset_id FROM tasks WHERE task_id=?1",
            [failed.task_id.to_string()],
            |r| r.get::<_, Option<String>>(0)
        )
        .unwrap(),
        None
    );
}

#[test]
fn changed_sources_and_abandoned_pending_imports_cannot_register_a_dataset() {
    use uuid::Uuid;
    let root = support::workspace_root("interrupted import Workspace");
    let source = support::workspace_root("interrupted import inputs");
    let image = source.join("pending.png");
    fs::write(&image, IMAGE).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Interrupted import".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let preview = |name: &str| {
        let grant = owner
            .grant_import_directory(session, source.clone())
            .unwrap();
        let plan = imports
            .inspect_directory(session, grant.grant_id, name.into())
            .unwrap();
        ImportConfirmation {
            plan_id: plan.plan_id,
            plan_fingerprint: plan.fingerprint,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
        }
    };
    let changed = preview("Changed");
    imports.submit_confirmation(session, changed, 2).unwrap();
    fs::write(&image, b"changed bytes").unwrap();
    let failure = imports.run_confirmation(session, changed, 3).unwrap();
    assert_eq!(failure.status, "FAILED");
    assert_eq!(failure.error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(failure.registration.is_none());
    fs::write(&image, IMAGE).unwrap();
    let abandoned = preview("Abandoned");
    let queued = imports.submit_confirmation(session, abandoned, 4).unwrap();
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let snapshot = imports
        .confirmation_task(reopened, queued.task_id, 5)
        .unwrap();
    assert_eq!(snapshot.status, "INTERRUPTED");
    assert_eq!(
        imports.run_confirmation(reopened, abandoned, 6).unwrap(),
        snapshot
    );
    assert!(
        owner
            .list_datasets(reopened, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    owner.close(reopened).unwrap();
}

#[test]
fn discovery_rejects_empty_and_oversized_selections_without_registering_a_dataset() {
    let workspace = support::workspace_root("bounded discovery Workspace");
    let source = support::workspace_root("bounded discovery inputs");
    let actor = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = actor.grant_selected_directory(workspace).unwrap();
    let session = actor
        .create(grant.grant_id, "Discovery limit".into(), 0)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&actor)).unwrap();
    let inspect = || {
        let grant = actor
            .grant_import_directory(session, source.clone())
            .unwrap();
        imports.inspect_directory(session, grant.grant_id, "Too large".into())
    };
    assert!(inspect().is_err());
    for index in 0..1001 {
        fs::write(source.join(format!("{index}.png")), IMAGE).unwrap();
    }
    assert!(inspect().is_err());
    assert!(
        actor
            .list_datasets(session, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    actor.close(session).unwrap();
}

#[cfg(windows)]
#[test]
fn directory_discovery_rejects_a_junction_without_following_external_inputs() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let workspace = support::workspace_root("junction discovery Workspace");
    let source = support::workspace_root("junction discovery inputs");
    let outside = support::workspace_root("junction discovery outside");
    fs::write(outside.join("private.png"), IMAGE).unwrap();
    let mut command = Command::new("powershell.exe");
    let created = command.arg("-NoProfile").arg("-NonInteractive").arg("-Command")
        .arg("New-Item -ItemType Junction -Path $env:DD_DISCOVERY_LINK -Target $env:DD_DISCOVERY_TARGET | Out-Null")
        .env("DD_DISCOVERY_LINK", source.join("redirected"))
        .env("DD_DISCOVERY_TARGET", &outside).creation_flags(0x08000000)
        .output().unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let actor = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = actor.grant_selected_directory(workspace).unwrap();
    let session = actor
        .create(grant.grant_id, "Junction discovery".into(), 0)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&actor)).unwrap();
    let grant = actor.grant_import_directory(session, source).unwrap();
    let Err(DatasetImportError::Source(error)) =
        imports.inspect_directory(session, grant.grant_id, "Reject junction".into())
    else {
        panic!("junction discovery was not rejected");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(outside.join("private.png")).unwrap(), IMAGE);
    actor.close(session).unwrap();
}

#[test]
fn selected_directory_discovers_images_deterministically_and_rejects_unknown_grants() {
    let workspace = support::workspace_root("discover Workspace");
    let source = support::workspace_root("discover inputs");
    fs::create_dir(source.join("nested")).unwrap();
    for name in ["z.PNG", "a.jpg", "nested/b.jpeg"] {
        fs::write(source.join(name), IMAGE).unwrap();
    }
    fs::write(source.join("ignored.txt"), b"not an image").unwrap();
    let actor = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = actor.grant_selected_directory(workspace).unwrap();
    let session = actor
        .create(grant.grant_id, "Discovery".into(), 0)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&actor)).unwrap();
    assert!(
        imports
            .inspect_directory(session, uuid::Uuid::new_v4(), "Sample".into())
            .is_err()
    );
    let grant = actor
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Sample".into())
        .unwrap();
    let names: Vec<_> = plan
        .preview
        .inputs
        .iter()
        .map(|input| input.image.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(names, ["a.jpg", "nested/b.jpeg", "z.PNG"]);
    assert!(
        actor
            .list_datasets(session, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        imports
            .inspect_directory(session, grant.grant_id, "Sample".into())
            .is_err()
    );
    assert_eq!(fs::read(source.join("z.PNG")).unwrap(), IMAGE);
    actor.close(session).unwrap();
}

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
