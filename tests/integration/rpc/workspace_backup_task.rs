use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dd_infra::workspace_backup::task::{BackupService, BackupSubmission};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use uuid::Uuid;

struct Fixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    target: PathBuf,
    owner: Arc<WorkspaceThread>,
    workspace_id: Uuid,
    session: Uuid,
}

impl Fixture {
    fn create() -> Self {
        let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap());
        let temp = tempfile::tempdir_in(run).unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let workspace = parent.join("Original");
        let target = parent.join("Backup destination");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&target).unwrap();
        let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
        let directory = owner.grant_selected_directory(workspace.clone()).unwrap();
        let opened = owner
            .create(directory.grant_id, "Backup fixture".into(), 100)
            .unwrap();
        Self {
            _temp: temp,
            workspace,
            target,
            owner,
            workspace_id: Uuid::parse_str(&opened.workspace_id).unwrap(),
            session: opened.workspace_session_id,
        }
    }

    fn submission(&self) -> BackupSubmission {
        let grant = self
            .owner
            .grant_backup_directory(self.session, self.target.clone())
            .unwrap();
        BackupSubmission {
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            destination_grant_id: grant.grant_id,
            include_reports: true,
        }
    }

    fn destination(&self, task_id: Uuid) -> PathBuf {
        self.target.join(format!("workspace-backup-{task_id}"))
    }

    fn reopen(&mut self) {
        self.owner.close(self.session).unwrap();
        let selection = self
            .owner
            .grant_selected_directory(self.workspace.clone())
            .unwrap();
        self.session = self
            .owner
            .open(selection.grant_id)
            .unwrap()
            .workspace_session_id;
    }
}

#[test]
fn backup_admission_is_scoped_and_idempotent_and_a_restored_snapshot_does_not_rerun_it() {
    let fixture = Fixture::create();
    let service = BackupService::new(Arc::clone(&fixture.owner));
    let submission = fixture.submission();
    let inside = fixture
        .owner
        .grant_backup_directory(fixture.session, fixture.workspace.clone())
        .unwrap();
    assert!(
        service
            .submit(
                fixture.session,
                BackupSubmission {
                    destination_grant_id: inside.grant_id,
                    ..submission
                },
                200
            )
            .is_err()
    );
    let export = fixture
        .owner
        .grant_export_directory(fixture.session, fixture.target.clone())
        .unwrap();
    assert!(
        service
            .submit(
                fixture.session,
                BackupSubmission {
                    destination_grant_id: export.grant_id,
                    ..submission
                },
                200
            )
            .is_err()
    );
    assert!(
        service
            .submit(
                fixture.session,
                BackupSubmission {
                    include_reports: false,
                    ..submission
                },
                200
            )
            .is_err()
    );
    let (queued, job) = service.submit(fixture.session, submission, 200).unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert!(queued.receipt.is_none());
    let (replayed, duplicate) = service.submit(fixture.session, submission, 202).unwrap();
    assert_eq!(replayed.task_id, queued.task_id);
    assert!(duplicate.is_none());
    assert!(
        service
            .submit(
                fixture.session,
                BackupSubmission {
                    destination_grant_id: fixture.submission().destination_grant_id,
                    ..submission
                },
                204
            )
            .is_err()
    );
    assert!(!fixture.destination(queued.task_id).exists());
    service
        .run_admitted(fixture.session, job.unwrap(), || 300)
        .unwrap();
    let saved = service
        .snapshot(fixture.session, queued.task_id, 400)
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "SUCCEEDED");
    let saved = saved.receipt.unwrap();
    assert_eq!(saved.package, fixture.destination(queued.task_id));
    assert_eq!(saved.workspace_id, fixture.workspace_id);
    assert!(saved.package.join("backup-manifest.json").is_file());
    assert_eq!(fs::read_dir(fixture.target).unwrap().count(), 1);

    let restored_root = fixture.workspace.parent().unwrap().join("Restored");
    let restored_owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let restored = restored_owner
        .restore_backup(saved.package.clone(), restored_root)
        .unwrap();
    let copied = BackupService::new(restored_owner)
        .snapshot(restored.workspace_session_id, queued.task_id, 500)
        .unwrap()
        .unwrap();
    assert_eq!(copied.status, "INTERRUPTED");
    assert_eq!(copied.error_code.as_deref(), Some("SESSION_ENDED"));
    assert!(copied.receipt.is_none());
    assert!(saved.package.join("backup-manifest.json").is_file());
}

#[test]
fn an_existing_destination_survives_and_the_backup_task_records_the_failure() {
    let fixture = Fixture::create();
    let service = BackupService::new(Arc::clone(&fixture.owner));
    let (task, job) = service
        .submit(fixture.session, fixture.submission(), 200)
        .unwrap();
    let destination = fixture.destination(task.task_id);
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("existing.txt"), b"keep").unwrap();
    service
        .run_admitted(fixture.session, job.unwrap(), || 300)
        .unwrap();
    let failed = service
        .snapshot(fixture.session, task.task_id, 400)
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("DESTINATION_EXISTS"));
    assert!(failed.receipt.is_none());
    assert_eq!(fs::read(destination.join("existing.txt")).unwrap(), b"keep");
    assert_eq!(fs::read_dir(fixture.target).unwrap().count(), 1);
}

#[test]
fn reopening_recovers_a_published_backup_only_when_its_actual_package_matches() {
    for tamper in [false, true] {
        let mut fixture = Fixture::create();
        let service = BackupService::new(Arc::clone(&fixture.owner));
        let (task, job) = service
            .submit(fixture.session, fixture.submission(), 200)
            .unwrap();
        let fault = Connection::open(fixture.workspace.join("workspace.db")).unwrap();
        fault
            .execute_batch(
                "CREATE TRIGGER fail_backup_finish BEFORE UPDATE OF status ON tasks
             WHEN OLD.operation_kind='WORKSPACE_BACKUP' AND NEW.status='SUCCEEDED'
             BEGIN SELECT RAISE(ABORT, 'synthetic final receipt failure'); END;",
            )
            .unwrap();
        assert!(
            service
                .run_admitted(fixture.session, job.unwrap(), || 300)
                .is_err()
        );
        let package = fixture.destination(task.task_id);
        assert!(package.join("backup-manifest.json").is_file());
        fault
            .execute_batch("DROP TRIGGER fail_backup_finish")
            .unwrap();
        drop(fault);
        if tamper {
            fs::write(package.join("workspace.db"), b"modified database bytes").unwrap();
        }
        fixture.reopen();
        let outcome = service
            .snapshot(fixture.session, task.task_id, 400)
            .unwrap()
            .unwrap();
        if tamper {
            assert_eq!(outcome.status, "INTERRUPTED");
            assert_eq!(
                outcome.error_code.as_deref(),
                Some("BACKUP_OUTPUT_UNVERIFIED")
            );
            assert!(outcome.receipt.is_none());
            assert_eq!(
                fs::read(package.join("workspace.db")).unwrap(),
                b"modified database bytes"
            );
        } else {
            assert_eq!(outcome.status, "SUCCEEDED");
            assert_eq!(outcome.receipt.unwrap().package, package);
        }
        assert_eq!(fs::read_dir(&fixture.target).unwrap().count(), 1);
    }
}
