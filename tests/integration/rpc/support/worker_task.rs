//! A real Core control endpoint, Workspace task and supervised Worker for RPC tests.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{GrantBook, GrantScope};
use dd_infra::execution_rpc::CoreClaimService;
use dd_infra::task_store::{TaskOutcome, TaskSubmission};
use dd_infra::worker_process::{CoreClaimEndpoint, WorkerLaunch, WorkerProcess};
use dd_infra::workspace_thread::WorkspaceThread;
use dd_protocol::v1::{
    ArtifactRef, ExecutionHandle, ExecutionLookup, ExecutionSnapshot, TaskContext, WorkerInputFile,
    artifact_ref, core_execution_control_server::CoreExecutionControlServer,
};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_stream::wrappers::TcpListenerStream;
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(20);
const NODE_FINGERPRINT: [u8; 32] = [0x23; 32];

pub struct WorkerTask {
    workspace: Arc<WorkspaceThread>,
    workspace_id: String,
    session: Uuid,
    executor: Uuid,
    attempt: Uuid,
    grants: Arc<Mutex<GrantBook>>,
    manifest: Vec<u8>,
    manifest_path: PathBuf,
    pub task_id: Uuid,
    pub output: PathBuf,
    pub fingerprint: [u8; 32],
}

impl WorkerTask {
    pub fn new(
        case: &str,
        operation: &str,
        capability: &str,
        asset_id: Uuid,
        sha: &[u8; 32],
        config: Value,
    ) -> Self {
        let root = run_root().join(case);
        fs::create_dir(&root).unwrap();
        let workspace = Arc::new(WorkspaceThread::start(250, DEADLINE).unwrap());
        let grant = workspace.grant_selected_directory(root.clone()).unwrap();
        let opened = workspace.create(grant.grant_id, case.into(), 100).unwrap();
        let session = opened.workspace_session_id;
        let fingerprint = [0x12; 32];
        let manifest = serde_json::to_vec(&json!({
            "dataset_id": null, "label_schema_digest": null,
            "assets": [{"asset_id": asset_id, "media_digest": hex(sha), "annotation_digest": null, "source_metadata_digest": null}],
            "resolved_config": config, "capability_id": capability, "implementation_version": "0.1.0",
            "model_identity": null, "input_fingerprint": hex(&fingerprint),
        })).unwrap();
        let manifest_path = root.join("input.json");
        fs::write(&manifest_path, &manifest).unwrap();
        let task = workspace
            .enqueue_task(
                TaskSubmission {
                    dataset_id: None,
                    workspace_session: session,
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                    operation_kind: operation.into(),
                    capability_id: capability.into(),
                    input_manifest_path: "input.json".into(),
                    request_fingerprint: [0x44; 32],
                    input_fingerprint: fingerprint,
                },
                200,
            )
            .unwrap();
        let attempt = Uuid::new_v4();
        let output = root.join(format!("artifacts/tasks/{}/{attempt}", task.task_id));
        fs::create_dir_all(&output).unwrap();
        Self {
            workspace,
            workspace_id: opened.workspace_id,
            session,
            executor: Uuid::new_v4(),
            attempt,
            grants: Arc::new(Mutex::new(GrantBook::new(session))),
            manifest,
            manifest_path,
            task_id: task.task_id,
            output,
            fingerprint,
        }
    }

    pub fn manifest_locator(&self) -> WorkerInputFile {
        file_locator(&self.manifest_path, &self.manifest)
    }

    pub fn dispatch(
        &self,
        method_domain: &[u8],
        input: &impl Message,
    ) -> (TaskContext, ArtifactRef) {
        let mut hash = Sha256::new();
        hash.update(method_domain);
        hash.update(input.encode_to_vec());
        let scope = GrantScope {
            workspace_session: self.session,
            task: self.task_id,
            attempt: self.attempt,
            executor_session: self.executor,
            input_fingerprint: self.fingerprint,
            operation_digest: hash.finalize().into(),
            reservation: Uuid::new_v4(),
        };
        let grant = self
            .grants
            .lock()
            .unwrap()
            .install(scope.clone(), Instant::now() + DEADLINE)
            .unwrap();
        self.workspace
            .record_dispatch(scope, grant, NODE_FINGERPRINT, 250)
            .unwrap();
        (
            TaskContext {
                workspace_id: self.workspace_id.clone(),
                workspace_session_id: self.session.to_string(),
                task_id: self.task_id.to_string(),
                attempt_id: self.attempt.to_string(),
                executor_session_id: self.executor.to_string(),
                input_fingerprint: hex(&self.fingerprint),
                node_spec_fingerprint: hex(&NODE_FINGERPRINT),
                grant_id: grant.to_string(),
                ..Default::default()
            },
            ArtifactRef {
                artifact_id: Uuid::new_v4().to_string(),
                owner: Some(artifact_ref::Owner::OwnerTaskId(self.task_id.to_string())),
                kind: "INPUT_MANIFEST".into(),
                size_bytes: self.manifest.len() as u64,
                media_type: "application/json".into(),
                sha256: hex(&digest(&self.manifest)),
                schema_id: None,
            },
        )
    }

    pub fn outcome(&self) -> TaskOutcome {
        self.workspace
            .task_outcome(self.session, self.task_id)
            .unwrap()
    }

    pub async fn launch(&self) -> LiveWorker {
        let claim_token = Uuid::new_v4();
        let core = CoreClaimService::new(
            self.executor,
            claim_token,
            Arc::clone(&self.grants),
            Arc::clone(&self.workspace),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(address) = listener.local_addr().unwrap() else {
            panic!("expected IPv4")
        };
        let (stop, stopping) = oneshot::channel();
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(CoreExecutionControlServer::new(core))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = stopping.await;
                })
                .await
                .unwrap();
        });
        let endpoint = Some(CoreClaimEndpoint {
            address,
            claim_token,
        });
        let worker = match std::env::var_os("DD_PACKAGED_WORKER_EXE") {
            Some(executable) => {
                let executable = PathBuf::from(executable);
                println!("Testing packaged Worker {}", executable.display());
                WorkerProcess::launch_packaged(executable, self.executor, endpoint).await
            }
            None => {
                WorkerProcess::launch(WorkerLaunch {
                    interpreter: PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap()),
                    entrypoint: Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../services/python-worker/src/dd_worker/entrypoint.py")
                        .canonicalize()
                        .unwrap(),
                    executor_session: self.executor,
                    core: endpoint,
                })
                .await
            }
        }
        .unwrap();
        LiveWorker {
            worker,
            stop,
            server,
        }
    }
}

pub struct LiveWorker {
    pub worker: WorkerProcess,
    stop: oneshot::Sender<()>,
    server: JoinHandle<()>,
}

impl LiveWorker {
    pub async fn wait(
        &mut self,
        handle: &ExecutionHandle,
        deadline: Duration,
    ) -> ExecutionSnapshot {
        timeout(deadline, async {
            loop {
                let lookup = ExecutionLookup {
                    task_id: handle.task_id.clone(),
                    attempt_id: handle.attempt_id.clone(),
                };
                let status = self.worker.execution(lookup).await.unwrap();
                if !matches!(status.status.as_str(), "QUEUED" | "RUNNING") {
                    return status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }

    pub async fn shutdown(self) {
        self.worker.shutdown().await.unwrap();
        self.stop.send(()).unwrap();
        self.server.await.unwrap();
    }
}

pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn run_root() -> PathBuf {
    let base = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/contract-check/worker-rpc")
        .canonicalize()
        .unwrap();
    assert_eq!(base.parent(), Some(expected.as_path()));
    base
}

fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn file_locator(path: &Path, bytes: &[u8]) -> WorkerInputFile {
    WorkerInputFile {
        path: path.to_str().unwrap().into(),
        sha256: hex(&digest(bytes)),
    }
}
