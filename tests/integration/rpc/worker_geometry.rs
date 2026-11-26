use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{ClaimReceipt, GrantBook, GrantScope};
use dd_infra::artifact_manifest::{
    ArtifactValidationError, TaskManifestScope, verify_task_result_manifest,
};
use dd_infra::execution_rpc::CoreClaimService;
use dd_infra::json::JsonContracts;
use dd_infra::task_store::{ResultSubmission, TaskStoreError, TaskSubmission};
use dd_infra::worker_process::{
    CoreClaimEndpoint, WorkerLaunch, WorkerProcess, WorkerProcessError,
};
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_protocol::v1::core_execution_control_server::CoreExecutionControl;
use dd_protocol::v1::{
    self as wire, ArtifactRef, ExecutionLookup, ExecutionStarted, ExecutionSucceeded,
    GeometryAuditInput, GeometryAuditRequest, ReportExecutionRequest, TaskContext, WorkerInputFile,
    report_execution_request::Event as ReportEvent,
};
use prost::Message;
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, Response, Status};
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
    let scope = GrantScope {
        workspace_session: session,
        task: task.task_id,
        attempt,
        executor_session: executor,
        input_fingerprint,
        operation_digest: geometry_digest(&input),
        reservation: Uuid::new_v4(),
    };
    let grant = grants
        .install(scope.clone(), Instant::now() + TIMEOUT)
        .unwrap();
    thread
        .record_dispatch(scope, grant, [0x34; 32], 250)
        .unwrap();

    let grants = Arc::new(Mutex::new(grants));
    let core = CoreClaimService::new(executor, core_key, Arc::clone(&grants), Arc::clone(&thread));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_address = listener.local_addr().unwrap();
    let (stop, stopping) = oneshot::channel::<()>();
    let core_server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(
                wire::core_execution_control_server::CoreExecutionControlServer::new(
                    DropFirstClaimAck {
                        core,
                        pending: AtomicBool::new(true),
                        pending_report: AtomicBool::new(true),
                        pending_failure: AtomicBool::new(true),
                    },
                ),
            )
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = stopping.await;
            })
            .await
            .unwrap();
    });
    let SocketAddr::V4(claim_address) = core_address else {
        panic!("expected IPv4 loopback")
    };
    let mut worker = WorkerProcess::launch(WorkerLaunch {
        interpreter: PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap()),
        entrypoint: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../services/python-worker/src/dd_worker/entrypoint.py")
            .canonicalize()
            .unwrap(),
        executor_session: executor,
        core: Some(CoreClaimEndpoint {
            address: claim_address,
            claim_token: core_key,
        }),
    })
    .await
    .unwrap();
    let health = worker.health().await.unwrap();
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
    let context = request.context.clone().unwrap();
    let mut altered = request.clone();
    altered.parameters.as_mut().unwrap().max_pixels = 101;
    assert!(
        matches!(worker.start_geometry(altered).await, Err(WorkerProcessError::Rpc(status)) if status.code() == Code::PermissionDenied)
    );
    assert!(fs::read_dir(&output).unwrap().next().is_none());
    let first = worker.start_geometry(request.clone()).await.unwrap();
    assert_eq!(worker.start_geometry(request.clone()).await.unwrap(), first);
    let snapshot = timeout(TIMEOUT, async {
        loop {
            let snapshot = worker
                .execution(ExecutionLookup {
                    task_id: first.task_id.clone(),
                    attempt_id: first.attempt_id.clone(),
                })
                .await
                .unwrap();
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

    {
        let db = Connection::open_with_flags(
            root.join("workspace.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let (status, path, revision): (String, Option<String>, i64) = db
            .query_row(
                "SELECT status, output_manifest_path, row_revision FROM tasks WHERE task_id=?1",
                [task.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            status, "SUCCEEDED",
            "Worker reported success before Core stored its result"
        );
        assert_eq!(
            path,
            Some(format!("{result_directory}/result-manifest.json"))
        );
        assert_eq!(revision, 4);
    }

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
    let changed = prepare_changed_source(&thread, &grants, &root, request);
    let changed_handle = worker.start_geometry(changed).await.unwrap();
    let failure = timeout(TIMEOUT, async {
        loop {
            let observed = worker
                .execution(ExecutionLookup {
                    task_id: changed_handle.task_id.clone(),
                    attempt_id: changed_handle.attempt_id.clone(),
                })
                .await
                .unwrap();
            assert_ne!(
                observed.status, "SUCCEEDED",
                "modified source produced an accepted result"
            );
            if observed.status == "FAILED" {
                break observed;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(failure.result_manifest_ref.is_none());
    {
        let db = Connection::open_with_flags(
            root.join("workspace.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let (status, code, output, revision): (String, Option<String>, Option<String>, i64) = db
            .query_row(
                "SELECT status, error_code, output_manifest_path, row_revision FROM tasks WHERE task_id=?1",
                [&changed_handle.task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            status, "FAILED",
            "Worker observed a failure but Core kept the task running"
        );
        assert_eq!(code.as_deref(), Some("INPUT_CHANGED"));
        assert!(output.is_none());
        assert_eq!(revision, 4);
        assert_eq!(failure.error_code, "input_changed");
        assert!(
            fs::read_dir(root.join(format!(
                "artifacts/tasks/{}/{}",
                changed_handle.task_id, changed_handle.attempt_id
            )))
            .unwrap()
            .next()
            .is_none()
        );
    }
    let mut reporter = wire::core_execution_control_client::CoreExecutionControlClient::connect(
        format!("http://{core_address}"),
    )
    .await
    .unwrap();
    let (reported_task, reported_directory, reported_event, accepted_at) = {
        let manual = prepare_report_fixture(
            &thread,
            &grants,
            &root,
            context,
            &published,
            &mut reporter,
            core_key,
        )
        .await;
        let context = manual.context;
        let execution = manual.execution;
        let grant = Uuid::parse_str(&context.grant_id).unwrap();
        let scope = TaskManifestScope {
            task: execution.task,
            attempt: execution.attempt,
            input_fingerprint,
        };
        let report_directory = format!("artifacts/tasks/{}/{}", execution.task, execution.attempt);
        let output = root.join(&report_directory);
        let manifest_ref = manual.reference;
        let verified =
            verify_task_result_manifest(&output, &contracts, &scope, &manifest_ref).unwrap();
        let done = ResultSubmission {
            workspace_session: session,
            grant,
            execution,
            node_spec_fingerprint: [0x34; 32],
            execution_event_id: Uuid::new_v4(),
            source_event_seq: 2,
            verified,
        };
        assert!(matches!(
            thread.record_success(done.clone(), 5000),
            Err(WorkspaceThreadError::Task(TaskStoreError::AttemptConflict))
        ));
        let completion = ReportExecutionRequest {
            context: Some(context),
            execution_event_id: done.execution_event_id.to_string(),
            worker_execution_id: execution.execution.to_string(),
            operation_digest: "44".repeat(32),
            source_event_seq: 2,
            event: Some(ReportEvent::Succeeded(ExecutionSucceeded {
                result_manifest_ref: Some(manifest_ref.clone()),
            })),
        };
        assert_eq!(
            reporter
                .report_execution(authorized(completion.clone(), Uuid::new_v4()))
                .await
                .unwrap_err()
                .code(),
            Code::Unauthenticated
        );
        let early = reporter
            .report_execution(authorized(completion.clone(), core_key))
            .await
            .unwrap_err();
        assert_eq!(early.code(), Code::FailedPrecondition);
        let started = ReportExecutionRequest {
            execution_event_id: Uuid::new_v4().to_string(),
            source_event_seq: 1,
            event: Some(ReportEvent::Started(ExecutionStarted {})),
            ..completion.clone()
        };
        let started = reporter
            .report_execution(authorized(started, core_key))
            .await
            .unwrap()
            .into_inner();
        assert!(started.accepted);
        assert!(started.terminal_receipt.is_none());
        let out_of_order = ReportExecutionRequest {
            source_event_seq: 1,
            ..completion.clone()
        };
        assert_eq!(
            reporter
                .report_execution(authorized(out_of_order, core_key))
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
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
        let mut bad_node = completion.clone();
        bad_node.context.as_mut().unwrap().node_spec_fingerprint = "ff".repeat(32);
        assert_eq!(
            reporter
                .report_execution(authorized(bad_node, core_key))
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
        let ack = reporter
            .report_execution(authorized(completion.clone(), core_key))
            .await
            .unwrap()
            .into_inner();
        assert!(ack.accepted);
        assert_eq!(ack.execution_event_id, done.execution_event_id.to_string());
        let saved = thread.record_success(done.clone(), 6000).unwrap();
        assert_eq!(
            ack.terminal_receipt.as_ref().unwrap().accepted_at,
            saved.accepted_at
        );
        assert_eq!(
            ack.terminal_receipt
                .as_ref()
                .unwrap()
                .completion_fingerprint,
            saved.completion_fingerprint
        );
        assert_eq!(
            reporter
                .report_execution(authorized(completion, core_key))
                .await
                .unwrap()
                .into_inner(),
            ack
        );
        let accepted_at = time::OffsetDateTime::parse(
            &saved.accepted_at,
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
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
        (
            execution.task,
            report_directory,
            done.execution_event_id,
            accepted_at,
        )
    };

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

    worker.shutdown().await.unwrap();
    drop(reporter);
    stop.send(()).unwrap();
    timeout(TIMEOUT, core_server).await.unwrap().unwrap();
    thread.close(session).unwrap();
    let db = Connection::open(root.join("workspace.db")).unwrap();
    let (status, path, finished, revision, event): (String, String, i64, i64, String) = db.query_row(
        "SELECT status, output_manifest_path, finished_at_ms, row_revision, json_extract(execution_metadata_json,'$.last_execution_event_id') FROM tasks WHERE task_id=?1",
        [reported_task.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).unwrap();
    assert_eq!(status, "SUCCEEDED");
    assert_eq!(path, format!("{reported_directory}/result-manifest.json"));
    assert_eq!(
        (finished, revision, event),
        (
            i64::try_from(accepted_at.unix_timestamp_nanos() / 1_000_000).unwrap(),
            4,
            reported_event.to_string()
        )
    );
}

struct DropFirstClaimAck {
    core: CoreClaimService,
    pending: AtomicBool,
    pending_report: AtomicBool,
    pending_failure: AtomicBool,
}

struct ReportFixture {
    context: TaskContext,
    execution: ClaimReceipt,
    reference: ArtifactRef,
}

fn prepare_changed_source(
    thread: &WorkspaceThread,
    grants: &Mutex<GrantBook>,
    root: &Path,
    mut request: GeometryAuditRequest,
) -> GeometryAuditRequest {
    let context = request.context.as_mut().unwrap();
    let session = Uuid::parse_str(&context.workspace_session_id).unwrap();
    let executor = Uuid::parse_str(&context.executor_session_id).unwrap();
    let task = thread
        .enqueue_task(
            TaskSubmission {
                workspace_session: session,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                operation_kind: "ANNOTATION_AUDIT".into(),
                capability_id: "annotation_geometry.v1".into(),
                input_manifest_path: "input.json".into(),
                request_fingerprint: [0x55; 32],
                input_fingerprint: [0x12; 32],
            },
            280,
        )
        .unwrap();
    let attempt = Uuid::new_v4();
    let output = root.join(format!("artifacts/tasks/{}/{attempt}", task.task_id));
    fs::create_dir_all(&output).unwrap();
    let original = root.join("changed-source.png");
    fs::write(&original, PNG).unwrap();
    let inputs = request.parameters.as_mut().unwrap();
    inputs.output_directory = output.to_str().unwrap().into();
    inputs.image = Some(locator(&original, PNG));
    let scope = GrantScope {
        workspace_session: session,
        task: task.task_id,
        attempt,
        executor_session: executor,
        input_fingerprint: [0x12; 32],
        operation_digest: geometry_digest(inputs),
        reservation: Uuid::new_v4(),
    };
    let grant = grants
        .lock()
        .unwrap()
        .install(scope.clone(), Instant::now() + TIMEOUT)
        .unwrap();
    thread
        .record_dispatch(scope, grant, [0x34; 32], 290)
        .unwrap();
    fs::write(original, b"modified after Core granted the frozen input").unwrap();
    context.task_id = task.task_id.to_string();
    context.attempt_id = attempt.to_string();
    context.grant_id = grant.to_string();
    request.input_manifest_ref.as_mut().unwrap().owner = Some(
        wire::artifact_ref::Owner::OwnerTaskId(context.task_id.clone()),
    );
    request
}

async fn prepare_report_fixture(
    thread: &WorkspaceThread,
    grants: &Mutex<GrantBook>,
    root: &Path,
    mut context: TaskContext,
    worker_manifest: &Value,
    reporter: &mut wire::core_execution_control_client::CoreExecutionControlClient<
        tonic::transport::Channel,
    >,
    credential: Uuid,
) -> ReportFixture {
    let previous_output = root.join(format!(
        "artifacts/tasks/{}/{}",
        context.task_id, context.attempt_id
    ));
    let session = Uuid::parse_str(&context.workspace_session_id).unwrap();
    let executor = Uuid::parse_str(&context.executor_session_id).unwrap();
    let queued = thread
        .enqueue_task(
            TaskSubmission {
                workspace_session: session,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                operation_kind: "REPORT_FIXTURE".into(),
                capability_id: "annotation_geometry.v1".into(),
                input_manifest_path: "input.json".into(),
                request_fingerprint: [0x22; 32],
                input_fingerprint: [0x12; 32],
            },
            300,
        )
        .unwrap();
    let attempt = Uuid::new_v4();
    let scope = GrantScope {
        workspace_session: session,
        task: queued.task_id,
        attempt,
        executor_session: executor,
        input_fingerprint: [0x12; 32],
        operation_digest: [0x44; 32],
        reservation: Uuid::new_v4(),
    };
    let grant = grants
        .lock()
        .unwrap()
        .install(scope.clone(), Instant::now() + TIMEOUT)
        .unwrap();
    thread
        .record_dispatch(scope, grant, [0x34; 32], 350)
        .unwrap();
    context.task_id = queued.task_id.to_string();
    context.attempt_id = attempt.to_string();
    context.grant_id = grant.to_string();
    let directory = root.join(format!("artifacts/tasks/{}/{attempt}", queued.task_id));
    fs::create_dir_all(&directory).unwrap();
    fs::copy(
        previous_output.join("geometry-result.json"),
        directory.join("geometry-result.json"),
    )
    .unwrap();
    let mut manifest = worker_manifest.clone();
    manifest["task_id"] = json!(context.task_id);
    manifest["attempt_id"] = json!(context.attempt_id);
    let manifest = serde_json::to_vec(&manifest).unwrap();
    fs::write(directory.join("result-manifest.json"), &manifest).unwrap();
    let execution = Uuid::new_v4();
    let claimed = reporter
        .claim_grant(authorized(
            wire::ClaimGrantRequest {
                grant_id: context.grant_id.clone(),
                workspace_session_id: context.workspace_session_id.clone(),
                task_id: context.task_id.clone(),
                attempt_id: context.attempt_id.clone(),
                executor_session_id: context.executor_session_id.clone(),
                worker_execution_id: execution.to_string(),
                input_fingerprint: context.input_fingerprint.clone(),
                operation_digest: "44".repeat(32),
            },
            credential,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(claimed.worker_execution_id, execution.to_string());
    let reference = ArtifactRef {
        artifact_id: Uuid::new_v4().to_string(),
        owner: Some(wire::artifact_ref::Owner::OwnerTaskId(
            context.task_id.clone(),
        )),
        kind: "TASK_RESULT_MANIFEST".into(),
        media_type: "application/json".into(),
        size_bytes: manifest.len().try_into().unwrap(),
        sha256: digest(&manifest),
        schema_id: None,
    };
    ReportFixture {
        context,
        execution: ClaimReceipt {
            task: queued.task_id,
            attempt,
            execution,
        },
        reference,
    }
}

#[tonic::async_trait]
impl CoreExecutionControl for DropFirstClaimAck {
    async fn claim_grant(
        &self,
        request: Request<wire::ClaimGrantRequest>,
    ) -> Result<Response<wire::ExecutionHandle>, Status> {
        let accepted = self.core.claim_grant(request).await?;
        if self.pending.swap(false, Ordering::Relaxed) {
            return Err(Status::unavailable(
                "injected response loss after durable claim",
            ));
        }
        Ok(accepted)
    }

    async fn report_execution(
        &self,
        request: Request<wire::ReportExecutionRequest>,
    ) -> Result<Response<wire::ReportAck>, Status> {
        let pending = match request.get_ref().event.as_ref() {
            Some(ReportEvent::Succeeded(_)) => Some(&self.pending_report),
            Some(ReportEvent::Failed(_)) => Some(&self.pending_failure),
            _ => None,
        };
        let accepted = self.core.report_execution(request).await?;
        if pending.is_some_and(|pending| pending.swap(false, Ordering::Relaxed)) {
            return Err(Status::unavailable(
                "injected response loss after durable completion",
            ));
        }
        Ok(accepted)
    }
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

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn geometry_digest(input: &GeometryAuditInput) -> [u8; 32] {
    let mut operation = Sha256::new();
    operation.update(b"datasetdoctor.v1.PythonWorker.StartAnnotationGeometryAudit\0");
    operation.update(input.encode_to_vec());
    operation.finalize().into()
}

fn locator(path: &Path, bytes: &[u8]) -> WorkerInputFile {
    WorkerInputFile {
        path: path.to_str().unwrap().into(),
        sha256: digest(bytes),
    }
}
