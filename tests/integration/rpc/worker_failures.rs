use std::{fs, sync::Arc, time::Duration};

use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use rusqlite::Connection;
use serde_json::{Value, json};
use tokio::time::timeout;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const LABELS: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"signal"}],"annotations":[]}"#;

#[tokio::test]
async fn failures_ack_loss_and_dead_workers_have_durable_authoritative_outcomes() {
    for (case, injection, reporting, status, error) in [
        (
            "Unexpected",
            "raise RuntimeError('private synthetic credential')",
            "lost_ack",
            "FAILED",
            "INTERNAL_ERROR",
        ),
        (
            "Publish",
            "Path(request.parameters.output_directory, 'geometry-result.json').write_bytes(b'occupied'); return original_compute(request)",
            "normal",
            "FAILED",
            "FILE_IO_FAILED",
        ),
        (
            "Unavailable",
            "raise RuntimeError('unavailable')",
            "unavailable",
            "INTERRUPTED",
            "WORKER_INTERRUPTED",
        ),
        (
            "Killed",
            "os._exit(19)",
            "normal",
            "INTERRUPTED",
            "WORKER_INTERRUPTED",
        ),
    ] {
        let fixture = WorkflowFixture::with_samples(case, LABELS, json!({}), &["signal.png"]);
        let owner = &fixture.workspace;
        let session = fixture.session;
        let source_bytes = fs::read(fixture.source.join("signal.json")).unwrap();
        let asset = owner
            .list_assets(session, fixture.dataset, None, 1)
            .unwrap()
            .items[0]
            .asset_id;
        let wrapper = fixture.root.join("injected-worker.py");
        // All fault injection lives in the ignored synthetic fixture, outside production code.
        let entry_directory =
            serde_json::to_string(&fixture.entrypoint.parent().unwrap().to_str().unwrap()).unwrap();
        let events =
            serde_json::to_string(&fixture.root.join("reported-events.jsonl").to_str().unwrap())
                .unwrap();
        fs::write(&wrapper, format!(r#"
import sys, os, json
from pathlib import Path
sys.path.insert(0, {entry_directory})
import entrypoint
import grpc
from worker_execution import core_execution_pb2_grpc
class LostAck(grpc.RpcError):
    def code(self):
        return grpc.StatusCode.UNAVAILABLE
original_stub = core_execution_pb2_grpc.CoreExecutionControlStub
def stub(channel):
    core = original_stub(channel)
    original_report = core.ReportExecution
    count = 0
    def report(event, **options):
        nonlocal count
        if event.source_event_seq == 2:
            count += 1
            with open({events}, 'a', encoding='utf-8') as output:
                output.write(json.dumps({{'id':event.execution_event_id,'seq':event.source_event_seq,'bytes':event.SerializeToString().hex()}}) + '\n')
            if '{reporting}' == 'unavailable':
                raise LostAck()
        ack = original_report(event, **options)
        if event.source_event_seq == 2 and '{reporting}' == 'lost_ack' and count == 1:
            raise LostAck()
        return ack
    core.ReportExecution = report
    return core
core_execution_pb2_grpc.CoreExecutionControlStub = stub
original_compute = entrypoint.geometry_execution.compute_result
def compute(request):
    {injection}
entrypoint.geometry_execution.compute_result = compute
raise SystemExit(entrypoint.main())
"#)).unwrap();
        let request = GeometryAuditRequest {
            dataset_id: fixture.dataset,
            asset_id: asset,
            max_pixels: 100,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
        };
        let queued = GeometryRuntime::admit(owner, session, request.clone()).unwrap();
        let task = queued.task().task_id;
        let mut runtime = GeometryRuntime::launch(
            Arc::clone(owner),
            session,
            fixture.interpreter.clone(),
            wrapper,
        )
        .await
        .unwrap();
        let result = timeout(Duration::from_secs(15), runtime.audit_admitted(queued))
            .await
            .unwrap();
        if status == "FAILED" {
            let outcome = result.unwrap();
            assert_eq!(outcome.status, status);
            assert_eq!(outcome.error_code.as_deref(), Some(error));
            assert_eq!(runtime.audit(request.clone()).await.unwrap(), outcome);
            runtime.shutdown().await.unwrap();
        } else {
            assert!(result.is_err());
            runtime.terminate().await.unwrap();
            GeometryRuntime::record_unavailable(owner, session, task, false).unwrap();
        }
        let persisted = owner.task_outcome(session, task).unwrap();
        assert_eq!(persisted.status, status);
        assert_eq!(persisted.error_code.as_deref(), Some(error));
        assert!(persisted.output_manifest_path.is_none());
        if reporting != "normal" {
            let log = fs::read_to_string(fixture.root.join("reported-events.jsonl")).unwrap();
            let events: Vec<Value> = log
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(events.len(), if reporting == "lost_ack" { 2 } else { 3 });
            assert!(events.iter().all(|event| event == &events[0]));
        }
        owner.close(session).unwrap();
        let grant = owner
            .grant_selected_directory(fixture.root.clone())
            .unwrap();
        let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
        assert_eq!(owner.task_outcome(reopened, task).unwrap().status, status);
        assert_eq!(
            GeometryRuntime::admit(owner, reopened, request)
                .unwrap()
                .task()
                .task_id,
            task
        );
        let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT attempt_no FROM tasks WHERE task_id=?1",
                [task.to_string()],
                |row| row.get::<_, u32>(0)
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
        assert_eq!(
            fs::read(fixture.source.join("signal.json")).unwrap(),
            source_bytes
        );
        owner.close(reopened).unwrap();
    }
}
