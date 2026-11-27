#[path = "../workspace/support.rs"]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{ClaimContext, GrantBook, GrantScope};
use dd_infra::json::JsonContracts;
use dd_infra::task_store::{TaskStoreError, TaskSubmission};
use dd_infra::workspace::{create_workspace, open_workspace};
use rusqlite::Connection;
use uuid::Uuid;

const WORKSPACE_ID: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const OLD_SESSION: &str = "76afdb2d-0900-4528-b303-0fca0f864235";

fn submission(workspace_session: Uuid) -> TaskSubmission {
    TaskSubmission {
        workspace_session,
        dataset_id: None,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        operation_kind: "ANNOTATION_AUDIT".into(),
        capability_id: "annotation_geometry.v1".into(),
        input_manifest_path: PathBuf::from("artifacts/tasks/frozen-input.json"),
        request_fingerprint: [0x12; 32],
        input_fingerprint: [0x34; 32],
    }
}

#[test]
fn queued_task_is_durable_and_deduplicates_replays_without_overwriting_the_first_request() {
    let root = support::workspace_root("task enqueued and reloaded");
    let contracts = JsonContracts::new().unwrap();
    let mut workspace = create_workspace(&root, &contracts, 250, "Task", WORKSPACE_ID, 0).unwrap();
    let initial = submission(Uuid::new_v4());
    let started = workspace.enqueue_task(&initial, 1234).unwrap();
    assert_eq!(started.status, "QUEUED");
    drop(workspace);

    let mut workspace = open_workspace(&root, &contracts, 250).unwrap();
    let mut replay = initial.clone();
    replay.request_id = Uuid::new_v4();
    replay.workspace_session = Uuid::new_v4();
    let observed = workspace.enqueue_task(&replay, 9999).unwrap();
    assert_eq!(observed, started);
    assert_eq!(observed.request_id, initial.request_id);
    assert_eq!(observed.workspace_session, initial.workspace_session);
    assert_eq!(observed.created_at_ms, 1234);
    replay.request_fingerprint = [0xcc; 32];
    assert!(matches!(
        workspace.enqueue_task(&replay, 9999),
        Err(TaskStoreError::IdempotencyConflict)
    ));
    replay.operation_kind = "IMAGE_INSPECTION".into();
    let separate = workspace.enqueue_task(&replay, 9999).unwrap();
    assert_ne!(started.task_id, separate.task_id);
    drop(workspace);

    let connection = Connection::open(root.join("workspace.db")).unwrap();
    let (status, attempt, digest, manifest): (String, Option<String>, String, String) = connection.query_row(
        "SELECT status, current_attempt_id, input_fingerprint, input_manifest_path FROM tasks WHERE task_id = ?1",
        [started.task_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(
        (status.as_str(), attempt, digest, manifest.as_str()),
        (
            "QUEUED",
            None,
            "34".repeat(32),
            "artifacts/tasks/frozen-input.json"
        )
    );
}

#[test]
fn manifest_path_must_stay_relative_to_the_workspace() {
    let root = support::workspace_root("task manifest path scope");
    let contracts = JsonContracts::new().unwrap();
    let mut workspace = create_workspace(&root, &contracts, 250, "Task", WORKSPACE_ID, 0).unwrap();
    let mut task = submission(Uuid::new_v4());
    for path in [
        root.join("input.json"),
        "artifacts/../../unapproved.json".into(),
        "tasks/file.json:stream".into(),
    ] {
        task.input_manifest_path = path;
        assert!(matches!(
            workspace.enqueue_task(&task, 10),
            Err(TaskStoreError::InvalidManifestPath)
        ));
    }
}

#[test]
fn current_attempt_persists_before_start_and_stale_events_cannot_move_it() {
    let root = support::workspace_root("task attempt ownership");
    let contracts = JsonContracts::new().unwrap();
    let mut workspace = create_workspace(&root, &contracts, 250, "Task", WORKSPACE_ID, 0).unwrap();
    let task = submission(Uuid::new_v4());
    let queued = workspace.enqueue_task(&task, 1000).unwrap();
    let mut scope = GrantScope {
        workspace_session: task.workspace_session,
        task: queued.task_id,
        attempt: Uuid::new_v4(),
        executor_session: Uuid::new_v4(),
        input_fingerprint: [0xff; 32],
        operation_digest: [0x56; 32],
        reservation: Uuid::new_v4(),
    };
    assert!(matches!(
        workspace.record_dispatch(&scope, Uuid::new_v4(), [0x78; 32], 2000),
        Err(TaskStoreError::AttemptConflict)
    ));
    scope.input_fingerprint = task.input_fingerprint;
    let now = Instant::now();
    let mut grants = GrantBook::new(scope.workspace_session);
    let grant = grants
        .install(scope.clone(), now + Duration::from_secs(1))
        .unwrap();
    workspace
        .record_dispatch(&scope, grant, [0x78; 32], 2000)
        .unwrap();
    assert!(matches!(
        workspace.record_dispatch(&scope, Uuid::new_v4(), [0x78; 32], 2001),
        Err(TaskStoreError::AttemptConflict)
    ));

    let receipt = grants
        .claim(
            grant,
            scope.executor_session,
            &ClaimContext {
                workspace_session: scope.workspace_session,
                task: scope.task,
                attempt: scope.attempt,
                execution: Uuid::new_v4(),
                input_fingerprint: scope.input_fingerprint,
                operation_digest: scope.operation_digest,
            },
            now,
        )
        .unwrap();
    assert!(matches!(
        workspace.record_running(Uuid::new_v4(), grant, receipt, 3000),
        Err(TaskStoreError::AttemptConflict)
    ));
    assert!(matches!(
        workspace.record_running(scope.workspace_session, Uuid::new_v4(), receipt, 3000),
        Err(TaskStoreError::AttemptConflict)
    ));
    assert!(matches!(
        workspace.record_running(scope.workspace_session, grant, receipt, 3000),
        Err(TaskStoreError::AttemptConflict)
    ));
    workspace
        .record_claimed(scope.workspace_session, grant, receipt, 2500)
        .unwrap();
    workspace
        .record_claimed(scope.workspace_session, grant, receipt, 2700)
        .unwrap();
    workspace
        .record_running(scope.workspace_session, grant, receipt, 3000)
        .unwrap();
    workspace
        .record_running(scope.workspace_session, grant, receipt, 9000)
        .unwrap();
    drop(workspace);

    let db = Connection::open(root.join("workspace.db")).unwrap();
    let (status, attempt, count, revision, began, metadata): (String, String, i64, i64, i64, String) = db.query_row(
        "SELECT status, current_attempt_id, attempt_no, row_revision, started_at_ms, execution_metadata_json FROM tasks", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).unwrap();
    assert_eq!(
        (status.as_str(), attempt, count, revision, began),
        ("RUNNING", scope.attempt.to_string(), 1, 3, 3000)
    );
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["grant_state"], "CLAIMED");
    assert_eq!(
        metadata["claimed_execution_id"],
        receipt.execution.to_string()
    );
    assert_eq!(
        metadata["resource_reservation_id"],
        scope.reservation.to_string()
    );
    assert_eq!(metadata["node_spec_fingerprint"], "78".repeat(32));
}

#[test]
fn killed_owner_leaves_attempts_recoverable_without_rescheduling_queued_tasks() {
    const CHILD_ROOT: &str = "DD_TASK_CRASH_CHILD_ROOT";
    let old_session = Uuid::parse_str(OLD_SESSION).unwrap();
    if let Some(path) = std::env::var_os(CHILD_ROOT) {
        run_crash_fixture(&PathBuf::from(path), old_session);
        unreachable!("fixture waits to be terminated by the parent process");
    }
    let root = support::workspace_root("Task owner crash 后恢复");
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "killed_owner_leaves_attempts_recoverable_without_rescheduling_queued_tasks",
        ])
        .env(CHILD_ROOT, &root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        child.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = StopOnDrop(child.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(12);
    while !root.join("ready").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "fixture exited before SQLite was ready"
        );
        assert!(
            Instant::now() < deadline,
            "fixture did not record dispatching and running tasks"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());

    let contracts = JsonContracts::new().unwrap();
    let mut workspace = open_workspace(&root, &contracts, 250).unwrap();
    assert_eq!(
        workspace
            .recover_abandoned_session(old_session, 5000)
            .unwrap(),
        0
    );
    assert_eq!(
        workspace
            .recover_abandoned_session(old_session, 6000)
            .unwrap(),
        0
    );
    drop(workspace);
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let mut rows = db.prepare("SELECT operation_kind, status, finished_at_ms, json_extract(execution_metadata_json,'$.grant_state') FROM tasks ORDER BY operation_kind").unwrap();
    let observed = rows
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let recovered_at = observed[0]
        .2
        .expect("automatic recovery persisted a terminal timestamp");
    assert!(recovered_at > 400);
    assert_eq!(
        observed,
        [
            (
                "DISPATCHING_FIXTURE".into(),
                "INTERRUPTED".into(),
                Some(recovered_at),
                Some("REVOKED".into())
            ),
            ("QUEUED_FIXTURE".into(), "QUEUED".into(), None, None),
            (
                "RUNNING_FIXTURE".into(),
                "INTERRUPTED".into(),
                Some(recovered_at),
                Some("REVOKED".into())
            ),
            (
                "UNRELATED_SESSION".into(),
                "INTERRUPTED".into(),
                Some(recovered_at),
                Some("REVOKED".into())
            ),
        ]
    );
}

#[test]
fn explicit_session_recovery_remains_scoped_and_idempotent() {
    let old_session = Uuid::parse_str(OLD_SESSION).unwrap();
    let root = support::workspace_root("Explicit scoped task recovery");
    let contracts = JsonContracts::new().unwrap();
    let mut workspace = create_workspace(&root, &contracts, 250, "Task", WORKSPACE_ID, 0).unwrap();
    seed_attempts(&mut workspace, old_session);
    assert_eq!(
        workspace
            .recover_abandoned_session(old_session, 5000)
            .unwrap(),
        2
    );
    assert_eq!(
        workspace
            .recover_abandoned_session(old_session, 6000)
            .unwrap(),
        0
    );
    drop(workspace);
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let mut rows = db.prepare("SELECT operation_kind, status, finished_at_ms, json_extract(execution_metadata_json,'$.grant_state') FROM tasks ORDER BY operation_kind").unwrap();
    let observed = rows
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        observed,
        [
            (
                "DISPATCHING_FIXTURE".into(),
                "INTERRUPTED".into(),
                Some(5000),
                Some("REVOKED".into())
            ),
            ("QUEUED_FIXTURE".into(), "QUEUED".into(), None, None),
            (
                "RUNNING_FIXTURE".into(),
                "INTERRUPTED".into(),
                Some(5000),
                Some("REVOKED".into())
            ),
            (
                "UNRELATED_SESSION".into(),
                "DISPATCHING".into(),
                None,
                Some("ISSUED".into())
            ),
        ]
    );
}

fn run_crash_fixture(root: &Path, session: Uuid) {
    let repository_runs = PathBuf::from(std::env::var_os("DD_WORKSPACE_LOCK_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed_runs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/p02-workspace/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(repository_runs.parent(), Some(allowed_runs.as_path()));
    assert_eq!(
        root.canonicalize().unwrap().parent(),
        Some(repository_runs.as_path())
    );
    let contracts = JsonContracts::new().unwrap();
    let mut workspace = create_workspace(root, &contracts, 250, "Task", WORKSPACE_ID, 0).unwrap();
    seed_attempts(&mut workspace, session);
    fs::write(root.join("ready"), b"committed").unwrap();
    loop {
        std::thread::park();
    }
}

fn seed_attempts(workspace: &mut dd_infra::workspace::OpenedWorkspace, session: Uuid) {
    for (operation, session) in [
        ("QUEUED_FIXTURE", session),
        ("DISPATCHING_FIXTURE", session),
        ("RUNNING_FIXTURE", session),
        ("UNRELATED_SESSION", Uuid::new_v4()),
    ] {
        let mut submission = submission(session);
        submission.operation_kind = operation.into();
        let task = workspace.enqueue_task(&submission, 100).unwrap();
        if operation == "QUEUED_FIXTURE" {
            continue;
        }
        let scope = GrantScope {
            workspace_session: session,
            task: task.task_id,
            attempt: Uuid::new_v4(),
            executor_session: Uuid::new_v4(),
            input_fingerprint: submission.input_fingerprint,
            operation_digest: [3; 32],
            reservation: Uuid::new_v4(),
        };
        let grant = Uuid::new_v4();
        workspace
            .record_dispatch(&scope, grant, [4; 32], 200)
            .unwrap();
        if operation == "RUNNING_FIXTURE" {
            let receipt = dd_core::execution_grant::ClaimReceipt {
                task: task.task_id,
                attempt: scope.attempt,
                execution: Uuid::new_v4(),
            };
            workspace
                .record_claimed(session, grant, receipt, 300)
                .unwrap();
            workspace
                .record_running(session, grant, receipt, 400)
                .unwrap();
        }
    }
}

struct StopOnDrop(Child);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
