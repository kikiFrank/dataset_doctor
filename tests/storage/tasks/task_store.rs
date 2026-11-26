#[path = "../workspace/support.rs"]
mod support;

use std::path::PathBuf;

use dd_infra::json::JsonContracts;
use dd_infra::task_store::{TaskStoreError, TaskSubmission};
use dd_infra::workspace::{create_workspace, open_workspace};
use rusqlite::Connection;
use uuid::Uuid;

const WORKSPACE_ID: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";

fn submission(workspace_session: Uuid) -> TaskSubmission {
    TaskSubmission {
        workspace_session,
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
