use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{ClaimReceipt, GrantBook, GrantScope};
use dd_infra::artifact_manifest::{
    ArtifactValidationError, TaskManifestScope, verify_task_result_manifest,
};
use dd_infra::execution_rpc::CoreClaimService;
use dd_infra::json::JsonContracts;
use dd_infra::task_store::{ResultSubmission, TaskStoreError, TaskSubmission};
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_protocol::v1::{
    self as wire, ArtifactRef, ExecutionLookup, GeometryAuditInput, GeometryAuditRequest,
    TaskContext, WorkerInputFile,
};
use prost::Message;
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(10);
const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[tokio::test]
async fn real_python_worker_checks_geometry_only_after_a_durable_core_grant() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/contract-check/worker-rpc")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let root = run.join("Geometry Workspace");
    fs::create_dir(&root).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, TIMEOUT).unwrap());
    let directory = thread.grant_selected_directory(root.clone()).unwrap();
    let workspace = thread
        .create(directory.grant_id, "Geometry".into(), 100)
        .unwrap();
    let session = workspace.workspace_session_id;
    let executor = Uuid::new_v4();
    let core_key = Uuid::new_v4();
    let worker_key = Uuid::new_v4();
    let mut grants = GrantBook::new(session);
    let image_path = root.join("synthetic.png");
    let annotation_path = root.join("synthetic.json");
    let manifest_path = root.join("input.json");
    let annotation = br#"{"images":[{"id":1,"file_name":"synthetic.png","width":2,"height":1}],"annotations":[{"id":7,"image_id":1,"category_id":1,"bbox":[0,0,3,1],"area":3,"iscrowd":0}],"categories":[{"id":1,"name":"helmet"}]}"#;
    let input_fingerprint = [0x12; 32];
    let manifest = serde_json::to_vec(&json!({
        "dataset_id": null, "label_schema_digest": null,
        "assets": [{"asset_id": Uuid::new_v4().to_string(), "media_digest": digest(PNG), "annotation_digest": digest(annotation), "source_metadata_digest": null}],
        "resolved_config": {"max_pixels": 100}, "capability_id": "annotation_geometry.v1", "implementation_version": "0.1.0", "model_identity": null,
        "input_fingerprint": "12".repeat(32),
    })).unwrap();
    fs::write(&image_path, PNG).unwrap();
    fs::write(&annotation_path, annotation).unwrap();
    fs::write(&manifest_path, &manifest).unwrap();
    let task = thread
        .enqueue_task(
            TaskSubmission {
                workspace_session: session,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                operation_kind: "ANNOTATION_AUDIT".into(),
                capability_id: "annotation_geometry.v1".into(),
                input_manifest_path: "input.json".into(),
                request_fingerprint: [0x22; 32],
                input_fingerprint,
            },
            200,
        )
        .unwrap();
    let attempt = Uuid::new_v4();
    let result_directory = format!("artifacts/tasks/{}/{attempt}", task.task_id);
    let output = root.join(&result_directory);
    fs::create_dir_all(&output).unwrap();
    let input = GeometryAuditInput {
        image: Some(locator(&image_path, PNG)),
        annotation: Some(locator(&annotation_path, annotation)),
        input_manifest: Some(locator(&manifest_path, &manifest)),
        output_directory: output.to_str().unwrap().into(),
        max_pixels: 100,
    };
    let mut operation = Sha256::new();
    operation.update(b"datasetdoctor.v1.PythonWorker.StartAnnotationGeometryAudit\0");
    operation.update(input.encode_to_vec());
    let scope = GrantScope {
        workspace_session: session,
        task: task.task_id,
        attempt,
        executor_session: executor,
        input_fingerprint,
        operation_digest: operation.finalize().into(),
        reservation: Uuid::new_v4(),
    };
    let grant = grants
        .install(scope.clone(), Instant::now() + TIMEOUT)
        .unwrap();
    thread
        .record_dispatch(scope, grant, [0x34; 32], 250)
        .unwrap();

    let core = CoreClaimService::new(
        executor,
        core_key,
        Arc::new(Mutex::new(grants)),
        Arc::clone(&thread),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_address = listener.local_addr().unwrap();
    let (stop, stopping) = oneshot::channel::<()>();
    let core_server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(wire::core_execution_control_server::CoreExecutionControlServer::new(core))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = stopping.await;
            })
            .await
            .unwrap();
    });
    let (mut child, port) =
        start_worker(executor, worker_key, &core_address.to_string(), core_key).await;
    let mut client = timeout(
        TIMEOUT,
        wire::python_worker_client::PythonWorkerClient::connect(format!("http://127.0.0.1:{port}")),
    )
    .await
    .unwrap()
    .unwrap();
    let health = client
        .health(authorized(wire::HealthRequest {}, worker_key))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(health.loaded_capability_ids, ["annotation_geometry.v1"]);

    let request = GeometryAuditRequest {
        context: Some(TaskContext {
            workspace_id: workspace.workspace_id,
            workspace_session_id: session.to_string(),
            task_id: task.task_id.to_string(),
            attempt_id: attempt.to_string(),
            executor_session_id: executor.to_string(),
            input_fingerprint: "12".repeat(32),
            node_spec_fingerprint: "34".repeat(32),
            grant_id: grant.to_string(),
            ..Default::default()
        }),
        input_manifest_ref: Some(ArtifactRef {
            artifact_id: Uuid::new_v4().to_string(),
            owner: Some(wire::artifact_ref::Owner::OwnerTaskId(
                task.task_id.to_string(),
            )),
            kind: "INPUT_MANIFEST".into(),
            size_bytes: manifest.len().try_into().unwrap(),
            media_type: "application/json".into(),
            sha256: digest(&manifest),
            schema_id: None,
        }),
        parameters: Some(input),
    };
    let mut altered = request.clone();
    altered.parameters.as_mut().unwrap().max_pixels = 101;
    assert_eq!(
        client
            .start_annotation_geometry_audit(authorized(altered, worker_key))
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
    assert!(fs::read_dir(&output).unwrap().next().is_none());
    let first = client
        .start_annotation_geometry_audit(authorized(request.clone(), worker_key))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        client
            .start_annotation_geometry_audit(authorized(request, worker_key))
            .await
            .unwrap()
            .into_inner(),
        first
    );
    let snapshot = timeout(TIMEOUT, async {
        loop {
            let snapshot = client
                .get_execution(authorized(
                    ExecutionLookup {
                        task_id: first.task_id.clone(),
                        attempt_id: first.attempt_id.clone(),
                    },
                    worker_key,
                ))
                .await
                .unwrap()
                .into_inner();
            if snapshot.status == "FAILED" {
                panic!("worker failed: {}", snapshot.error_code);
            }
            if snapshot.status == "SUCCEEDED" {
                break snapshot;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let manifest_ref = snapshot.result_manifest_ref.unwrap();
    let written = fs::read(output.join("result-manifest.json")).unwrap();
    assert_eq!(manifest_ref.sha256, digest(&written));
    let published: Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(published["task_id"], task.task_id.to_string());
    assert_eq!(published["attempt_id"], attempt.to_string());
    assert_eq!(published["input_fingerprint"], "12".repeat(32));
    let checked = fs::read(output.join("geometry-result.json")).unwrap();
    assert_eq!(published["entries"][0]["sha256"], digest(&checked));
    let checked: Value = serde_json::from_slice(&checked).unwrap();
    assert_eq!(
        checked["findings"][0],
        json!({"code": "bbox_out_of_bounds", "annotation_id": 7})
    );
    assert_eq!(fs::read(&image_path).unwrap(), PNG);
    assert_eq!(fs::read(&annotation_path).unwrap(), annotation);

    let contracts = JsonContracts::new().unwrap();
    let scope = TaskManifestScope {
        task: task.task_id,
        attempt,
        input_fingerprint,
    };
    let verified = verify_task_result_manifest(&output, &contracts, &scope, &manifest_ref).unwrap();
    assert_eq!(verified.entries().len(), 1);
    assert_eq!(verified.entries()[0].relative_path, "geometry-result.json");
    let stale = TaskManifestScope {
        attempt: Uuid::new_v4(),
        ..scope
    };
    assert!(matches!(
        verify_task_result_manifest(&output, &contracts, &stale, &manifest_ref),
        Err(ArtifactValidationError::ScopeMismatch)
    ));
    let execution = ClaimReceipt {
        task: task.task_id,
        attempt,
        execution: Uuid::parse_str(&first.worker_execution_id).unwrap(),
    };
    let done = ResultSubmission {
        workspace_session: session,
        grant,
        execution,
        node_spec_fingerprint: [0x34; 32],
        execution_event_id: Uuid::new_v4(),
        verified,
    };
    assert!(matches!(
        thread.record_success(done.clone(), 5000),
        Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict))
    ));
    thread
        .record_running(session, grant, execution, 4000)
        .unwrap();
    let stale_session = ResultSubmission {
        workspace_session: Uuid::new_v4(),
        ..done.clone()
    };
    assert!(matches!(
        thread.record_success(stale_session, 5000),
        Err(WorkspaceThreadError::Access(_))
    ));
    let stale_node = ResultSubmission {
        node_spec_fingerprint: [0xff; 32],
        ..done.clone()
    };
    assert!(matches!(
        thread.record_success(stale_node, 5000),
        Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict))
    ));
    let saved = thread.record_success(done.clone(), 5000).unwrap();
    assert_eq!(saved.accepted_at, "1970-01-01T00:00:05Z");
    assert_eq!(thread.record_success(done.clone(), 6000).unwrap(), saved);
    let conflicting = ResultSubmission {
        execution_event_id: Uuid::new_v4(),
        ..done.clone()
    };
    assert!(matches!(
        thread.record_success(conflicting, 7000),
        Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict))
    ));
    let replaced_reference = ArtifactRef {
        artifact_id: Uuid::new_v4().to_string(),
        ..manifest_ref.clone()
    };
    let replaced = ResultSubmission {
        verified: verify_task_result_manifest(&output, &contracts, &scope, &replaced_reference)
            .unwrap(),
        ..done.clone()
    };
    assert!(matches!(
        thread.record_success(replaced, 7000),
        Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict))
    ));

    let result_path = output.join("geometry-result.json");
    fs::write(&result_path, b"changed after publication").unwrap();
    assert!(matches!(
        verify_task_result_manifest(&output, &contracts, &scope, &manifest_ref),
        Err(ArtifactValidationError::DigestMismatch)
    ));
    let mut escaped = published;
    escaped["entries"][0]["relative_path"] = json!("../synthetic.json");
    let escaped = serde_json::to_vec(&escaped).unwrap();
    fs::write(output.join("result-manifest.json"), &escaped).unwrap();
    let forged = ArtifactRef {
        sha256: digest(&escaped),
        size_bytes: escaped.len().try_into().unwrap(),
        ..manifest_ref
    };
    assert!(matches!(
        verify_task_result_manifest(&output, &contracts, &scope, &forged),
        Err(ArtifactValidationError::InvalidEntryPath)
    ));

    assert!(
        client
            .shutdown(authorized(wire::ShutdownRequest {}, worker_key))
            .await
            .unwrap()
            .into_inner()
            .accepted
    );
    assert!(
        timeout(TIMEOUT, child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    stop.send(()).unwrap();
    timeout(TIMEOUT, core_server).await.unwrap().unwrap();
    thread.close(session).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let (status, path, finished, revision, event): (String, String, i64, i64, String) = db.query_row(
        "SELECT status, output_manifest_path, finished_at_ms, row_revision, json_extract(execution_metadata_json,'$.last_execution_event_id') FROM tasks WHERE task_id=?1",
        [task.task_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).unwrap();
    assert_eq!(status, "SUCCEEDED");
    assert_eq!(path, format!("{result_directory}/result-manifest.json"));
    assert_eq!(
        (finished, revision, event),
        (5000, 4, done.execution_event_id.to_string())
    );
}

fn authorized<T>(message: T, token: Uuid) -> Request<T> {
    let mut request = Request::new(message);
    request.set_timeout(TIMEOUT);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", token.simple()).parse().unwrap(),
    );
    request
}

async fn start_worker(
    executor: Uuid,
    token: Uuid,
    core_address: &str,
    core_key: Uuid,
) -> (Child, u64) {
    let python = std::env::var_os("DD_WORKER_PYTHON").unwrap();
    let entrypoint = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/python-worker/src/dd_worker/entrypoint.py");
    let mut worker = Command::new(python)
        .args(["-I", "-B"])
        .arg(entrypoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let bootstrap = json!({"token": token.simple().to_string(), "executor_session_id": executor.to_string(), "protocol_major": 1,
        "core": {"address": core_address, "claim_token": core_key.simple().to_string()}});
    let mut input = worker.stdin.take().unwrap();
    input
        .write_all(format!("{bootstrap}\n").as_bytes())
        .await
        .unwrap();
    drop(input);
    let mut output = BufReader::new(worker.stdout.take().unwrap());
    let mut ready = String::new();
    timeout(TIMEOUT, output.read_line(&mut ready))
        .await
        .unwrap()
        .unwrap();
    let ready: Value = serde_json::from_str(&ready).expect("Worker startup frame");
    (worker, ready["port"].as_u64().unwrap())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn locator(path: &Path, bytes: &[u8]) -> WorkerInputFile {
    WorkerInputFile {
        path: path.to_str().unwrap().into(),
        sha256: digest(bytes),
    }
}
