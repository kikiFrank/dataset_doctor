use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{GrantBook, GrantScope};
use dd_infra::execution_rpc::CoreClaimService;
use dd_protocol::v1::ClaimGrantRequest;
use dd_protocol::v1::core_execution_control_client::CoreExecutionControlClient;
use dd_protocol::v1::core_execution_control_server::CoreExecutionControlServer;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(5);

fn request(claim: ClaimGrantRequest, token: Option<Uuid>) -> Request<ClaimGrantRequest> {
    let mut request = Request::new(claim);
    request.set_timeout(TIMEOUT);
    if let Some(token) = token {
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", token.simple()).parse().unwrap(),
        );
    }
    request
}

#[tokio::test]
async fn actual_grpc_claim_uses_authenticated_executor_and_returns_the_original_handle() {
    let workspace = Uuid::new_v4();
    let executor = Uuid::new_v4();
    let mut grants = GrantBook::new(workspace);
    let scope = GrantScope {
        workspace_session: workspace,
        task: Uuid::new_v4(),
        attempt: Uuid::new_v4(),
        executor_session: executor,
        input_fingerprint: [0x12; 32],
        operation_digest: [0x34; 32],
        reservation: Uuid::new_v4(),
    };
    let grant = grants
        .install(scope.clone(), Instant::now() + Duration::from_secs(30))
        .unwrap();
    let credential = Uuid::new_v4();
    let service = CoreClaimService::new(executor, credential, Arc::new(Mutex::new(grants)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopping) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(CoreExecutionControlServer::new(service))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = stopping.await;
            })
            .await
            .unwrap();
    });
    let mut client = tokio::time::timeout(
        TIMEOUT,
        CoreExecutionControlClient::connect(format!("http://{address}")),
    )
    .await
    .unwrap()
    .unwrap();
    let mut claim = ClaimGrantRequest {
        grant_id: grant.to_string(),
        workspace_session_id: workspace.to_string(),
        task_id: scope.task.to_string(),
        attempt_id: scope.attempt.to_string(),
        executor_session_id: executor.to_string(),
        worker_execution_id: Uuid::new_v4().to_string(),
        input_fingerprint: "12".repeat(32),
        operation_digest: "34".repeat(32),
    };
    assert_eq!(
        client
            .claim_grant(request(claim.clone(), None))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthenticated
    );
    assert_eq!(
        client
            .claim_grant(request(claim.clone(), Some(Uuid::new_v4())))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthenticated
    );
    claim.executor_session_id = Uuid::new_v4().to_string();
    assert_eq!(
        client
            .claim_grant(request(claim.clone(), Some(credential)))
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
    claim.executor_session_id = executor.to_string();
    claim.input_fingerprint = "not-sha256".into();
    assert_eq!(
        client
            .claim_grant(request(claim.clone(), Some(credential)))
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    claim.input_fingerprint = "12".repeat(32);
    let first = python_claim(address.to_string(), credential, &claim).await;
    let replay = client
        .claim_grant(request(claim.clone(), Some(credential)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first["task_id"], replay.task_id);
    assert_eq!(first["attempt_id"], replay.attempt_id);
    assert_eq!(first["worker_execution_id"], replay.worker_execution_id);
    assert_eq!(replay.task_id, claim.task_id);
    assert_eq!(replay.attempt_id, claim.attempt_id);
    assert_eq!(replay.worker_execution_id, claim.worker_execution_id);
    assert_eq!(
        client
            .claim_grant(request(claim.clone(), Some(credential)))
            .await
            .unwrap()
            .into_inner(),
        replay
    );
    claim.worker_execution_id = Uuid::new_v4().to_string();
    assert_eq!(
        client
            .claim_grant(request(claim, Some(credential)))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );

    drop(client);
    stop.send(()).unwrap();
    tokio::time::timeout(TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
}

async fn python_claim(address: String, credential: Uuid, claim: &ClaimGrantRequest) -> Value {
    let python =
        std::env::var_os("DD_WORKER_PYTHON").expect("set DD_WORKER_PYTHON to the locked Python");
    let probe = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/integration/rpc/core_claim_wire.py");
    let mut child = Command::new(python)
        .args(["-I", "-B"])
        .arg(probe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let bootstrap = json!({
        "address": address,
        "credential": credential.simple().to_string(),
        "claim": {
            "grant_id": claim.grant_id, "workspace_session_id": claim.workspace_session_id,
            "task_id": claim.task_id, "attempt_id": claim.attempt_id,
            "executor_session_id": claim.executor_session_id, "worker_execution_id": claim.worker_execution_id,
            "input_fingerprint": claim.input_fingerprint, "operation_digest": claim.operation_digest,
        }
    });
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(format!("{bootstrap}\n").as_bytes())
        .await
        .unwrap();
    drop(input);
    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "Python Core client did not successfully claim its grant"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
