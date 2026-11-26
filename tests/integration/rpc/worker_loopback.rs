use std::env;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use dd_protocol::PROTOCOL_MAJOR;
use dd_protocol::v1::{
    ArtifactRef, HealthRequest, ShutdownRequest, python_worker_client::PythonWorkerClient,
};
use prost::Message;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use tonic::{Code, Request};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(5);
const ARTIFACT_CASES: &str =
    include_str!("../../../testdata/contracts/proto/v1/artifact-ref/cases.json");

#[tokio::test]
async fn rust_observes_the_real_python_worker_and_shuts_it_down() {
    let python =
        env::var_os("DD_WORKER_PYTHON").expect("set DD_WORKER_PYTHON to the locked Python");
    let entrypoint = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/python-worker/src/dd_worker/entrypoint.py");
    let token = Uuid::new_v4().simple().to_string();
    let session = Uuid::new_v4().to_string();
    let bootstrap = json!({
        "protocol_major": PROTOCOL_MAJOR,
        "executor_session_id": session,
        "token": token
    });
    let mut child = Command::new(python)
        .args(["-I", "-B"])
        .arg(entrypoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("launch the Python Worker");
    let mut stdin = child.stdin.take().unwrap();
    let line = format!("{bootstrap}\n");
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.shutdown().await.unwrap();
    drop(stdin);
    let stdout = child.stdout.take().unwrap();
    let mut ready = String::new();
    let mut output = BufReader::new(stdout);
    timeout(DEADLINE, output.read_line(&mut ready))
        .await
        .expect("Worker startup timed out")
        .unwrap();
    assert!(
        !ready.is_empty(),
        "Worker exited before announcing its port"
    );
    assert!(
        !ready.contains(&token),
        "Worker exposed the bootstrap token"
    );
    let reply: Value = serde_json::from_str(&ready).expect("Worker announces structured readiness");
    let port: u16 = reply["port"].as_u64().unwrap().try_into().unwrap();
    assert_ne!(port, 0);
    let mut client = timeout(
        DEADLINE,
        PythonWorkerClient::connect(format!("http://127.0.0.1:{port}")),
    )
    .await
    .expect("gRPC connection timed out")
    .expect("connect to the Worker loopback port");
    let denied = client
        .health(request(HealthRequest {}, None))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), Code::Unauthenticated);
    let denied = client
        .shutdown(request(ShutdownRequest {}, Some("bad-token")))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), Code::Unauthenticated);
    let health = client
        .health(request(HealthRequest {}, Some(&token)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(health.role, "WORKER");
    assert_eq!(health.executor_session_id, session);
    assert_eq!(health.protocol_major, PROTOCOL_MAJOR);
    assert_eq!(health.build_version, "0.1.0");
    assert!(health.loaded_capability_ids.is_empty());
    assert_eq!(health.active_executions, 0);
    let receipt = client
        .shutdown(request(ShutdownRequest {}, Some(&token)))
        .await
        .unwrap()
        .into_inner();
    assert!(receipt.accepted);
    let exit = timeout(DEADLINE, child.wait())
        .await
        .expect("Worker did not exit after shutdown")
        .unwrap();
    assert!(exit.success(), "Worker shutdown status: {exit}");
}

fn request<M>(message: M, token: Option<&str>) -> Request<M> {
    let mut request = Request::new(message);
    request.set_timeout(DEADLINE);
    if let Some(token) = token {
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    request
}

#[tokio::test]
async fn rust_and_python_generated_artifact_refs_preserve_wire_values() {
    let python =
        env::var_os("DD_WORKER_PYTHON").expect("set DD_WORKER_PYTHON to the locked Python");
    let probe =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/rpc/worker_wire.py");
    let cases: Value = serde_json::from_str(ARTIFACT_CASES).unwrap();
    for case in cases["valid"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let wire = hex_bytes(case["wire_hex"].as_str().unwrap());
        let original = ArtifactRef::decode(wire.as_slice()).unwrap();
        let mut child = Command::new(&python)
            .args(["-I", "-B"])
            .arg(&probe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&original.encode_to_vec()).await.unwrap();
        drop(stdin);
        let output = timeout(DEADLINE, child.wait_with_output())
            .await
            .expect("Python Protobuf probe timed out")
            .unwrap();
        assert!(output.status.success(), "{id}");
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["fields"], case["expected_fields"], "{id}");
        let returned = hex_bytes(reply["wire_hex"].as_str().unwrap());
        assert_eq!(
            ArtifactRef::decode(returned.as_slice()).unwrap(),
            original,
            "{id}"
        );
    }
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
