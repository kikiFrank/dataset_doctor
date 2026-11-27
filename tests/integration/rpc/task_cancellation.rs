use std::{fs, sync::Arc};

use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use serde_json::json;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

#[cfg(windows)]
#[path = "support/process_handle.rs"]
mod process_handle;

#[cfg(windows)]
#[tokio::test]
async fn an_exited_worker_cannot_leave_its_descendant_alive() {
    let fixture = WorkflowFixture::with_samples("CancelCrash", LABELS, json!({}), &["signal.png"]);
    let asset = fixture
        .workspace
        .list_assets(fixture.session, fixture.dataset, None, 1)
        .unwrap()
        .items[0]
        .asset_id;
    let marker = fixture.root.join("descendant.pid");
    let release = fixture.root.join("crash-now");
    let wrapper = fixture.root.join("crashing-worker.py");
    let directory =
        serde_json::to_string(fixture.entrypoint.parent().unwrap().to_str().unwrap()).unwrap();
    fs::write(
        &wrapper,
        format!(
            r#"
import sys, os, time, subprocess
from pathlib import Path
sys.path.insert(0, {directory})
import entrypoint
def compute(request):
    child = subprocess.Popen([sys.executable, '-I', '-B', '-c', 'import time; time.sleep(60)'],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        creationflags=subprocess.CREATE_NO_WINDOW)
    temporary = Path({marker}).with_suffix('.tmp')
    temporary.write_text(str(child.pid), encoding='ascii')
    os.replace(temporary, {marker})
    end = time.monotonic() + 10
    while not Path({release}).exists() and time.monotonic() < end:
        time.sleep(0.01)
    os._exit(19)
entrypoint.geometry_execution.compute_result = compute
raise SystemExit(entrypoint.main())
"#,
            marker = serde_json::to_string(marker.to_str().unwrap()).unwrap(),
            release = serde_json::to_string(release.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let job = GeometryRuntime::admit(
        &fixture.workspace,
        fixture.session,
        GeometryAuditRequest {
            dataset_id: fixture.dataset,
            asset_id: asset,
            max_pixels: 100,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
        },
    )
    .unwrap();
    let task = job.task().task_id;
    let mut worker = GeometryRuntime::launch(
        Arc::clone(&fixture.workspace),
        fixture.session,
        fixture.interpreter.clone(),
        wrapper,
    )
    .await
    .unwrap();
    let execution = tokio::spawn(async move {
        let result = worker.audit_admitted(job).await;
        worker.terminate().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let child = process_handle::SyntheticProcess::open(
        fs::read_to_string(marker).unwrap().parse().unwrap(),
    );
    assert_eq!(
        fixture
            .workspace
            .cancel_task(fixture.session, task, 8)
            .unwrap()
            .observed_status,
        "CANCELLING"
    );
    fs::write(release, b"crash").unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(8), execution)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let stopped = child.stopped();
    drop(child); // Always reap the synthetic process even when the red assertion fails.
    assert!(
        stopped,
        "terminated supervisor must reap the exited worker's descendants"
    );
    // Simulate Core losing its final reconciliation after the worker has died.
    // Reopening under the exclusive Workspace lock must settle this abandoned attempt.
    fixture.workspace.close(fixture.session).unwrap();
    let grant = fixture
        .workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = fixture
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        fixture
            .workspace
            .task_outcome(reopened, task)
            .unwrap()
            .status,
        "INTERRUPTED"
    );
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
    let db = rusqlite::Connection::open(fixture.root.join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT attempt_no FROM tasks WHERE task_id=?1",
            [task.to_string()],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

const LABELS: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"signal"}],"annotations":[]}"#;

#[tokio::test]
async fn selected_success_and_failure_win_the_late_cancellation_race() {
    for (case, failure, expected) in [
        ("CancelRaceSuccess", false, "SUCCEEDED"),
        ("CancelRaceFailure", true, "FAILED"),
    ] {
        let fixture = WorkflowFixture::with_samples(case, LABELS, json!({}), &["signal.png"]);
        let asset = fixture
            .workspace
            .list_assets(fixture.session, fixture.dataset, None, 1)
            .unwrap()
            .items[0]
            .asset_id;
        let wrapper = fixture.root.join("terminal-barrier-worker.py");
        let directory =
            serde_json::to_string(fixture.entrypoint.parent().unwrap().to_str().unwrap()).unwrap();
        let selected = fixture.root.join("terminal-selected");
        let delivered = fixture.root.join("late-cancel");
        let release = fixture.root.join("release-terminal");
        fs::write(
            &wrapper,
            format!(
                r#"
import sys, time, json
from pathlib import Path
sys.path.insert(0, {directory})
import entrypoint
original_compute = entrypoint.geometry_execution.compute_result
def compute(request):
    result = original_compute(request)
    if {failure}:
        raise ValueError('synthetic invalid input')
    return result
entrypoint.geometry_execution.compute_result = compute
original_report = entrypoint.WorkerExecutions._report
def report(self, event):
    if event.source_event_seq == 2:
        Path({selected}).touch()
        end = time.monotonic() + 10
        while not Path({release}).exists() and time.monotonic() < end:
            time.sleep(0.01)
    return original_report(self, event)
entrypoint.WorkerExecutions._report = report
original_cancel = entrypoint.WorkerExecutions.cancel
def cancel(self, lookup, rpc):
    receipt = original_cancel(self, lookup, rpc)
    assert not receipt.requested and receipt.observed_status == 'RUNNING'
    Path({delivered}).touch()
    return receipt
entrypoint.WorkerExecutions.cancel = cancel
raise SystemExit(entrypoint.main())
"#,
                failure = if failure { "True" } else { "False" },
                selected = serde_json::to_string(selected.to_str().unwrap()).unwrap(),
                delivered = serde_json::to_string(delivered.to_str().unwrap()).unwrap(),
                release = serde_json::to_string(release.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        let job = GeometryRuntime::admit(
            &fixture.workspace,
            fixture.session,
            GeometryAuditRequest {
                dataset_id: fixture.dataset,
                asset_id: asset,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
        let task = job.task().task_id;
        let mut worker = GeometryRuntime::launch(
            Arc::clone(&fixture.workspace),
            fixture.session,
            fixture.interpreter.clone(),
            wrapper,
        )
        .await
        .unwrap();
        let execution = tokio::spawn(async move {
            let result = worker.audit_admitted(job).await;
            if result.is_ok() {
                worker.shutdown().await.unwrap();
            } else {
                worker.terminate().await.unwrap();
            }
            result
        });
        for marker in [&selected, &delivered] {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                while !marker.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if marker == &selected {
                assert_eq!(
                    fixture
                        .workspace
                        .cancel_task(fixture.session, task, 30)
                        .unwrap()
                        .observed_status,
                    "CANCELLING"
                );
            }
        }
        fs::write(release, b"release").unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(8), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome.status, expected);
        let receipt = fixture
            .workspace
            .cancel_task(fixture.session, task, 31)
            .unwrap();
        assert!(!receipt.requested);
        assert_eq!(receipt.observed_status, expected);
        assert_eq!(
            fixture
                .workspace
                .cancel_task(fixture.session, task, 32)
                .unwrap(),
            receipt
        );
        fixture.workspace.close(fixture.session).unwrap();
        let grant = fixture
            .workspace
            .grant_selected_directory(fixture.root.clone())
            .unwrap();
        let reopened = fixture
            .workspace
            .open(grant.grant_id)
            .unwrap()
            .workspace_session_id;
        assert_eq!(
            fixture
                .workspace
                .task_outcome(reopened, task)
                .unwrap()
                .status,
            expected
        );
    }
}

#[tokio::test]
async fn running_image_cancellation_is_confirmed_by_the_real_worker_and_survives_reopen() {
    let fixture = WorkflowFixture::with_samples("CancelImage", LABELS, json!({}), &["signal.png"]);
    let asset = fixture
        .workspace
        .list_assets(fixture.session, fixture.dataset, None, 1)
        .unwrap()
        .items[0]
        .asset_id;
    let wrapper = fixture.root.join("gated-worker.py");
    let directory =
        serde_json::to_string(fixture.entrypoint.parent().unwrap().to_str().unwrap()).unwrap();
    let ready = fixture.root.join("computed");
    let release = fixture.root.join("release");
    let delivered = fixture.root.join("cancel-delivered");
    fs::write(
        &wrapper,
        format!(
            r#"
import sys, time
from pathlib import Path
sys.path.insert(0, {directory})
import entrypoint
original = entrypoint.geometry_execution.compute_result
import grpc, worker_execution
original_stub = worker_execution.core_execution_pb2_grpc.CoreExecutionControlStub
def stub(channel):
    core = original_stub(channel)
    original_report = core.ReportExecution
    lost = False
    def report(event, **options):
        nonlocal lost
        if event.WhichOneof('event') == 'cancelled':
            with open({events}, 'a', encoding='ascii') as output:
                output.write(event.SerializeToString().hex() + '\n')
        ack = original_report(event, **options)
        if event.WhichOneof('event') == 'cancelled' and not lost:
            lost = True
            class LostAck(grpc.RpcError):
                def code(self): return grpc.StatusCode.UNAVAILABLE
            raise LostAck()
        return ack
    core.ReportExecution = report
    return core
worker_execution.core_execution_pb2_grpc.CoreExecutionControlStub = stub
original_cancel = entrypoint.WorkerExecutions.cancel
def cancel(self, lookup, rpc):
    receipt = original_cancel(self, lookup, rpc)
    Path({delivered}).touch()
    return receipt
entrypoint.WorkerExecutions.cancel = cancel
def compute(request):
    result = original(request)
    Path({ready}).touch()
    end = time.monotonic() + 10
    while not Path({release}).exists() and time.monotonic() < end:
        time.sleep(0.01)
    return result
entrypoint.geometry_execution.compute_result = compute
raise SystemExit(entrypoint.main())
"#,
            ready = serde_json::to_string(ready.to_str().unwrap()).unwrap(),
            delivered = serde_json::to_string(delivered.to_str().unwrap()).unwrap(),
            events =
                serde_json::to_string(fixture.root.join("cancel-events.txt").to_str().unwrap())
                    .unwrap(),
            release = serde_json::to_string(release.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let job = GeometryRuntime::admit(
        &fixture.workspace,
        fixture.session,
        GeometryAuditRequest {
            dataset_id: fixture.dataset,
            asset_id: asset,
            max_pixels: 100,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
        },
    )
    .unwrap();
    let task = job.task().task_id;
    let mut worker = GeometryRuntime::launch(
        Arc::clone(&fixture.workspace),
        fixture.session,
        fixture.interpreter.clone(),
        wrapper,
    )
    .await
    .unwrap();
    let workspace = Arc::clone(&fixture.workspace);
    let session = fixture.session;
    let execution = tokio::spawn(async move {
        let outcome = worker.audit_admitted(job).await;
        if outcome.is_ok() {
            worker.shutdown().await.unwrap();
        } else {
            worker.terminate().await.unwrap();
        }
        outcome
    });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !ready.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        workspace.close(session).is_err(),
        "a live executor must retain Workspace ownership until stopped"
    );
    assert_eq!(
        workspace.current().unwrap().unwrap().workspace_session_id,
        session
    );
    let cancellation = workspace.cancel_task(session, task, 20).unwrap();
    assert_eq!(cancellation.observed_status, "CANCELLING");
    assert_eq!(
        workspace.cancel_task(session, task, 21).unwrap(),
        cancellation
    );
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !delivered.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fs::write(release, b"release").unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(8), execution)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.status, "CANCELLED");
    assert!(outcome.output_manifest_path.is_none());
    let events = fs::read_to_string(fixture.root.join("cancel-events.txt")).unwrap();
    let events: Vec<_> = events.lines().collect();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0], events[1],
        "lost cancellation acknowledgement must replay exactly the same event"
    );
    assert!(workspace.cancel_task(session, task, 22).unwrap().requested);
    workspace.close(session).unwrap();
    let grant = workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = workspace.open(grant.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        workspace.task_outcome(reopened, task).unwrap().status,
        "CANCELLED"
    );
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
}

#[tokio::test]
async fn queued_cancellation_is_durable_idempotent_and_never_dispatches() {
    let fixture = WorkflowFixture::with_samples("CancelQueued", LABELS, json!({}), &["signal.png"]);
    let asset = fixture
        .workspace
        .list_assets(fixture.session, fixture.dataset, None, 1)
        .unwrap()
        .items[0]
        .asset_id;
    let request = GeometryAuditRequest {
        dataset_id: fixture.dataset,
        asset_id: asset,
        max_pixels: 100,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let queued =
        GeometryRuntime::admit(&fixture.workspace, fixture.session, request.clone()).unwrap();
    let task = queued.task().task_id;
    let receipt = fixture
        .workspace
        .cancel_task(fixture.session, task, 10)
        .unwrap();
    assert!(receipt.requested);
    assert_eq!(receipt.observed_status, "CANCELLED");
    assert_eq!(
        fixture
            .workspace
            .cancel_task(fixture.session, task, 11)
            .unwrap(),
        receipt
    );
    assert!(
        fixture
            .workspace
            .cancel_task(Uuid::new_v4(), task, 12)
            .is_err()
    );
    assert!(
        fixture
            .workspace
            .cancel_task(fixture.session, Uuid::new_v4(), 12)
            .is_err()
    );
    let mut worker = GeometryRuntime::launch(
        Arc::clone(&fixture.workspace),
        fixture.session,
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        worker.audit_admitted(queued).await.unwrap().status,
        "CANCELLED"
    );
    worker.shutdown().await.unwrap();
    assert!(
        !fixture
            .root
            .join(format!("artifacts/tasks/{task}"))
            .exists()
    );
    fixture.workspace.close(fixture.session).unwrap();
    let grant = fixture
        .workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let session = fixture
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        fixture
            .workspace
            .task_outcome(session, task)
            .unwrap()
            .status,
        "CANCELLED"
    );
    assert_eq!(
        fixture.workspace.cancel_task(session, task, 13).unwrap(),
        receipt
    );
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        LABELS
    );
}
