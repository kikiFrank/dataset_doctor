use std::process::Stdio;
use std::time::Duration;

use dd_protocol::PROTOCOL_MAJOR;
use dd_protocol::v1::{
    AgentCapabilitiesRequest, HealthRequest, ShutdownRequest,
    agent_service_client::AgentServiceClient,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use tonic::{Code, Request};
use uuid::Uuid;

#[tokio::test]
async fn the_shipped_agent_binary_authenticates_loopback_and_shuts_down() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dd-agent-service"));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().unwrap();
    let token = Uuid::new_v4().simple().to_string();
    let session = Uuid::new_v4().to_string();
    let payload =
        json!({"token": token, "executor_session_id": session, "protocol_major": PROTOCOL_MAJOR});
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(format!("{payload}\n").as_bytes())
        .await
        .unwrap();
    input.shutdown().await.unwrap();
    drop(input);
    let mut output = BufReader::new(child.stdout.take().unwrap()).take(4097);
    let mut ready = Vec::new();
    timeout(
        Duration::from_secs(10),
        output.read_until(b'\n', &mut ready),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ready.ends_with(b"\n") && ready.len() <= 4096);
    let port = serde_json::from_slice::<Value>(&ready).unwrap()["port"]
        .as_u64()
        .unwrap();
    assert!(port > 0 && port <= u16::MAX.into());
    let mut service = AgentServiceClient::connect(format!("http://127.0.0.1:{port}"))
        .await
        .unwrap();

    assert_eq!(
        service.health(HealthRequest {}).await.unwrap_err().code(),
        Code::Unauthenticated
    );
    assert_eq!(
        service
            .shutdown(ShutdownRequest {})
            .await
            .unwrap_err()
            .code(),
        Code::Unauthenticated
    );
    assert_eq!(
        service
            .get_capabilities(authorize(
                AgentCapabilitiesRequest {},
                &Uuid::new_v4().simple().to_string()
            ))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthenticated
    );
    let health = service
        .health(authorize(HealthRequest {}, &token))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(health.role, "AGENT");
    assert_eq!(health.executor_session_id, session);
    assert_eq!(health.protocol_major, PROTOCOL_MAJOR);
    assert_eq!(health.active_executions, 0);
    let supported = service
        .get_capabilities(authorize(AgentCapabilitiesRequest {}, &token))
        .await
        .unwrap()
        .into_inner();
    assert!(!supported.model_configured);
    assert_eq!(supported.protocol_major, PROTOCOL_MAJOR);
    assert!(
        supported
            .templates
            .iter()
            .all(|item| !item.execution_available)
    );
    let mut templates: Vec<_> = supported
        .templates
        .into_iter()
        .map(|item| (item.template_id, item.contract_version))
        .collect();
    templates.sort_unstable();
    assert_eq!(
        templates,
        [
            ("industrial_dataset_healthcheck.v1".to_owned(), 1),
            ("repair_recheck.v1".to_owned(), 1),
            ("video_to_dataset.v1".to_owned(), 1),
        ]
    );
    assert!(
        service
            .shutdown(authorize(ShutdownRequest {}, &token))
            .await
            .unwrap()
            .into_inner()
            .accepted
    );
    assert!(
        timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

fn authorize<T>(body: T, token: &str) -> Request<T> {
    let mut request = Request::new(body);
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
}
