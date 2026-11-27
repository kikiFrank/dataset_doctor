use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{
    FinalOperationState, ReviewFinalError, ReviewFinalRequest, ReviewFinalService,
};
use dd_infra::workflow_store::{LocalHealthcheckRequest, LocalHealthcheckRuntime};
use serde_json::json;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const LABELS: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"signal"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

async fn prepared_review(case: &str) -> (WorkflowFixture, ReviewFinalRequest) {
    let fixture = WorkflowFixture::with_samples(case, LABELS, json!({}), &["signal.png"]);
    let owner = &fixture.workspace;
    let session = fixture.session;
    let runtime = LocalHealthcheckRuntime::new(
        Arc::clone(owner),
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .unwrap();
    let initial = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 1,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let run = initial.workflow.run_id;
    runtime.run(initial.job.unwrap()).await.unwrap();
    let geometry = fixture
        .store
        .get(session, run)
        .unwrap()
        .geometry_tasks
        .remove(0);
    let review = owner
        .open_review(session, fixture.dataset, geometry.asset_id, now())
        .unwrap();
    let category = owner
        .dataset_detail(session, fixture.dataset)
        .unwrap()
        .categories
        .remove(0);
    let mut fixed: serde_json::Value = serde_json::from_slice(LABELS).unwrap();
    fixed["categories"] = json!([{"id":category.id,"name":category.name}]);
    fixed["annotations"][0]["category_id"] = json!(category.id);
    fixed["annotations"][0]["bbox"] = json!([0, 0, 2, 1]);
    fixed["annotations"][0]["area"] = json!(2);
    let draft = ReviewDraftService::new(Arc::clone(owner))
        .unwrap()
        .save(
            session,
            ReviewDraftRequest {
                review_id: review.review_id,
                source_task_id: geometry.task_id,
                expected_review_revision: review.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&fixed).unwrap(),
            },
            now(),
        )
        .unwrap();
    let request = ReviewFinalRequest {
        review_id: review.review_id,
        expected_review_revision: draft.review_revision,
        base_image_digest: review.base_image_digest,
        base_annotation_digest: review.base_annotation_digest,
        draft_digest: draft.draft_digest,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        confirmed_negative: false,
    };
    (fixture, request)
}

#[tokio::test]
async fn import_read_access_cannot_authorize_an_external_annotation_write() {
    let (fixture, request) = prepared_review("External Read Only Grant").await;
    let finals = ReviewFinalService::new(Arc::clone(&fixture.workspace)).unwrap();
    assert!(matches!(
        finals.prepare(fixture.session, request.clone(), now()),
        Err(ReviewFinalError::WriteAuthorizationRequired)
    ));
    let read = fixture
        .workspace
        .grant_import_directory(fixture.session, fixture.source.clone())
        .unwrap();
    assert!(
        finals
            .prepare_with_write_grant(fixture.session, request, read.grant_id, now())
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
    let db = rusqlite::Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM file_operations", [], |row| row
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    fixture.workspace.close(fixture.session).unwrap();
}

#[tokio::test]
async fn an_explicit_scoped_grant_publishes_final_and_preserves_reopen_replay() {
    let (fixture, request) = prepared_review("External Authorized Grant").await;
    let owner = &fixture.workspace;
    let session = fixture.session;
    let finals = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let access = finals.write_access(session, request.review_id).unwrap();
    assert!(access.authorization_required);
    assert!(!access.read_only);
    assert!(
        finals
            .authorize_write(session, request.review_id, fixture.root.clone())
            .is_err()
    );
    let grant = finals
        .authorize_write(session, request.review_id, fixture.source.clone())
        .unwrap();
    let operation = finals
        .prepare_with_write_grant(session, request.clone(), grant.grant_id, now())
        .unwrap();
    assert_eq!(operation.state, FinalOperationState::Prepared);
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
    assert_eq!(
        finals
            .resume(session, operation.operation_id, now())
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    let bytes = fs::read(fixture.source.join("signal.json")).unwrap();
    assert_ne!(bytes, LABELS);
    owner.close(session).unwrap();
    let selected = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(selected.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        finals.prepare(reopened, request, now()).unwrap().state,
        FinalOperationState::Committed
    );
    assert_eq!(fs::read(fixture.source.join("signal.json")).unwrap(), bytes);
    owner.close(reopened).unwrap();
}

#[tokio::test]
async fn an_authorized_negative_review_creates_only_its_missing_sidecar() {
    let fixture = WorkflowFixture::with_samples(
        "External New Sidecar",
        LABELS,
        json!({}),
        &["unannotated.png"],
    );
    let owner = &fixture.workspace;
    let session = fixture.session;
    let runtime = LocalHealthcheckRuntime::new(
        Arc::clone(owner),
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .unwrap();
    let initial = runtime
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 1,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let run = initial.workflow.run_id;
    runtime.run(initial.job.unwrap()).await.unwrap();
    let geometry = fixture
        .store
        .get(session, run)
        .unwrap()
        .geometry_tasks
        .remove(0);
    let review = owner
        .open_review(session, fixture.dataset, geometry.asset_id, now())
        .unwrap();
    assert!(review.base_annotation_digest.is_none());
    let document = json!({"images":[{"id":1,"file_name":"unannotated.png","width":2,"height":1}],"categories":[],"annotations":[]});
    let saved = ReviewDraftService::new(Arc::clone(owner))
        .unwrap()
        .save(
            session,
            ReviewDraftRequest {
                review_id: review.review_id,
                source_task_id: geometry.task_id,
                expected_review_revision: review.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&document).unwrap(),
            },
            now(),
        )
        .unwrap();
    let finals = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let grant = finals
        .authorize_write(session, review.review_id, fixture.source.clone())
        .unwrap();
    let operation = finals
        .prepare_with_write_grant(
            session,
            ReviewFinalRequest {
                review_id: review.review_id,
                expected_review_revision: saved.review_revision,
                base_image_digest: review.base_image_digest,
                base_annotation_digest: None,
                draft_digest: saved.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: true,
            },
            grant.grant_id,
            now(),
        )
        .unwrap();
    let sidecar = fixture.source.join("unannotated.json");
    assert!(!sidecar.exists());
    assert_eq!(
        finals
            .resume(session, operation.operation_id, now())
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(sidecar).unwrap()).unwrap(),
        document
    );
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
    fixture.workspace.close(session).unwrap();
}

#[tokio::test]
async fn readonly_annotation_rejects_before_intent_and_can_be_reauthorized() {
    let (fixture, request) = prepared_review("External Readonly Annotation").await;
    let finals = ReviewFinalService::new(Arc::clone(&fixture.workspace)).unwrap();
    let path = fixture.source.join("signal.json");
    let original_permissions = fs::metadata(&path).unwrap().permissions();
    let grant = finals
        .authorize_write(fixture.session, request.review_id, fixture.source.clone())
        .unwrap();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    let result =
        finals.prepare_with_write_grant(fixture.session, request.clone(), grant.grant_id, now());
    let access = finals
        .write_access(fixture.session, request.review_id)
        .unwrap();
    fs::set_permissions(&path, original_permissions).unwrap();
    assert!(
        matches!(result, Err(ReviewFinalError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert!(access.read_only);
    assert_eq!(fs::read(&path).unwrap(), LABELS);
    let db = rusqlite::Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM file_operations", [], |row| row
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
    let renewed = finals
        .authorize_write(fixture.session, request.review_id, fixture.source.clone())
        .unwrap();
    let operation = finals
        .prepare_with_write_grant(fixture.session, request, renewed.grant_id, now())
        .unwrap();
    assert_eq!(
        finals
            .resume(fixture.session, operation.operation_id, now())
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    fixture.workspace.close(fixture.session).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn a_real_windows_file_lock_is_reported_at_write_authorization() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let (fixture, request) = prepared_review("External Locked Annotation").await;
    let finals = ReviewFinalService::new(Arc::clone(&fixture.workspace)).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(fixture.source.join("signal.json"))
        .unwrap();
    let rejected =
        finals.authorize_write(fixture.session, request.review_id, fixture.source.clone());
    drop(file);
    assert!(matches!(rejected, Err(ReviewFinalError::FileBusy)));
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
    assert!(
        finals
            .authorize_write(fixture.session, request.review_id, fixture.source.clone())
            .is_ok()
    );
    fixture.workspace.close(fixture.session).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn real_ntfs_create_denial_rejects_the_write_probe() {
    use std::os::windows::process::CommandExt;
    use std::{path::PathBuf, process::Command};
    fn hidden_command(program: &str) -> Command {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new(program);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    struct RestoreAcl {
        directory: PathBuf,
        sid: String,
    }
    impl Drop for RestoreAcl {
        fn drop(&mut self) {
            let _ = hidden_command("icacls.exe")
                .arg(&self.directory)
                .args(["/remove:d", &self.sid])
                .output();
        }
    }
    let (fixture, request) = prepared_review("External ACL Denied").await;
    let directory = fixture.source.canonicalize().unwrap();
    let allowed = fixture.root.parent().unwrap().canonicalize().unwrap();
    assert!(directory.starts_with(&allowed));
    assert_ne!(directory, allowed);
    let who = hidden_command("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    assert!(who.status.success());
    let text = String::from_utf8_lossy(&who.stdout);
    let sid = text
        .trim()
        .rsplit(',')
        .next()
        .unwrap()
        .trim_matches('"')
        .to_owned();
    assert!(sid.starts_with("S-1-"));
    // icacls requires '*' before a numeric SID.
    // https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/icacls
    let sid = format!("*{sid}");
    let restore = RestoreAcl {
        directory: directory.clone(),
        sid: sid.clone(),
    };
    let denied = hidden_command("icacls.exe")
        .arg(&directory)
        .args(["/deny", &format!("{sid}:(WD,AD)")])
        .output()
        .unwrap();
    assert!(denied.status.success());
    let finals = ReviewFinalService::new(Arc::clone(&fixture.workspace)).unwrap();
    let rejected = finals.authorize_write(fixture.session, request.review_id, directory.clone());
    drop(restore);
    assert!(
        matches!(rejected, Err(ReviewFinalError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert_eq!(fs::read(directory.join("signal.json")).unwrap(), LABELS);
    assert!(
        finals
            .authorize_write(fixture.session, request.review_id, directory)
            .is_ok()
    );
    fixture.workspace.close(fixture.session).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn a_junction_cannot_redirect_an_already_selected_write_scope() {
    use std::os::windows::process::CommandExt;
    use std::{path::PathBuf, process::Command};
    struct RestoreDirectory {
        source: PathBuf,
        saved: PathBuf,
    }
    impl Drop for RestoreDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir(&self.source);
            let _ = fs::rename(&self.saved, &self.source);
        }
    }
    let (fixture, request) = prepared_review("External Junction Write Scope").await;
    let finals = ReviewFinalService::new(Arc::clone(&fixture.workspace)).unwrap();
    let grant = finals
        .authorize_write(fixture.session, request.review_id, fixture.source.clone())
        .unwrap();
    let run = fixture.root.parent().unwrap().canonicalize().unwrap();
    let saved = run.join("Junction Original Directory");
    let outside = run.join("Junction Outside Directory");
    assert!(
        fixture.source.starts_with(&run) && saved.starts_with(&run) && outside.starts_with(&run)
    );
    assert_ne!(fixture.source, run);
    fs::create_dir(&outside).unwrap();
    fs::copy(
        fixture.source.join("signal.png"),
        outside.join("signal.png"),
    )
    .unwrap();
    fs::copy(
        fixture.source.join("signal.json"),
        outside.join("signal.json"),
    )
    .unwrap();
    fs::rename(&fixture.source, &saved).unwrap();
    let restore = RestoreDirectory {
        source: fixture.source.clone(),
        saved,
    };
    let created = Command::new("powershell.exe")
        .creation_flags(0x0800_0000)
        .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:DD_TEST_LINK -Target $env:DD_TEST_DEST | Out-Null"])
        .env("DD_TEST_LINK", fixture.source.to_string_lossy().trim_start_matches(r"\\?\"))
        .env("DD_TEST_DEST", outside.to_string_lossy().trim_start_matches(r"\\?\"))
        .output().unwrap();
    assert!(created.status.success());
    let rejected = finals.prepare_with_write_grant(fixture.session, request, grant.grant_id, now());
    drop(restore);
    assert!(rejected.is_err());
    assert_eq!(fs::read(outside.join("signal.json")).unwrap(), LABELS);
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
    fixture.workspace.close(fixture.session).unwrap();
}
