use std::net::Ipv4Addr;
use std::time::Duration;

use dd_agent_service::model::{AgentModelClient, ModelCall, ModelError, ModelPurpose};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Debug, Deserialize, Eq, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
struct Answer {
    sample_limit: u32,
}

#[tokio::test]
async fn rig_requests_one_typed_response_using_the_reserved_output_budget() {
    let (endpoint, observed) = provider(r#"{"sample_limit":4}"#, Duration::ZERO).await;
    let profile = Uuid::new_v4();
    let client = AgentModelClient::new_openai_chat(
        profile,
        endpoint,
        "synthetic-key".into(),
        "test-model".into(),
    )
    .unwrap();
    let request = ModelCall {
        profile_id: profile,
        purpose: ModelPurpose::Plan,
        reservation_id: Uuid::new_v4(),
        max_output_tokens: 64,
        deadline: Duration::from_secs(3),
    };
    assert!(matches!(
        client
            .request::<Answer>(
                ModelCall {
                    profile_id: Uuid::new_v4(),
                    ..request
                },
                "outside profile"
            )
            .await,
        Err(ModelError::InvalidReservation)
    ));
    let result = client
        .request::<Answer>(request, "Synthetic dataset, no image data")
        .await
        .unwrap();
    assert_eq!(result.output, Answer { sample_limit: 4 });
    assert_eq!(result.output_tokens, Some(12));
    let body = observed.await.unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["response_format"]["type"], "json_schema");
}

#[tokio::test]
async fn rig_deadline_and_malformed_output_do_not_implicitly_retry() {
    let (endpoint, observed) = provider(r#"{"sample_limit":4}"#, Duration::from_secs(1)).await;
    let profile = Uuid::new_v4();
    let client = AgentModelClient::new_openai_chat(
        profile,
        endpoint,
        "synthetic-key".into(),
        "test-model".into(),
    )
    .unwrap();
    let request = ModelCall {
        profile_id: profile,
        purpose: ModelPurpose::VisualReview,
        reservation_id: Uuid::new_v4(),
        max_output_tokens: 32,
        deadline: Duration::from_millis(400),
    };
    assert!(matches!(
        client.request::<Answer>(request, "Synthetic summary").await,
        Err(ModelError::Deadline)
    ));
    let seen = tokio::time::timeout(Duration::from_secs(4), observed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen["model"], "test-model");

    let (endpoint, observed) =
        provider(r#"{"sample_limit":4,"shell":"unexpected"}"#, Duration::ZERO).await;
    let client = AgentModelClient::new_openai_chat(
        profile,
        endpoint,
        "synthetic-key".into(),
        "test-model".into(),
    )
    .unwrap();
    let request = ModelCall {
        deadline: Duration::from_secs(2),
        reservation_id: Uuid::new_v4(),
        ..request
    };
    assert!(matches!(
        client.request::<Answer>(request, "Synthetic summary").await,
        Err(ModelError::InvalidOutput)
    ));
    observed.await.unwrap();
}

async fn provider(answer: &'static str, delay: Duration) -> (String, JoinHandle<Value>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        drop(listener);
        let mut stream = BufReader::new(connection);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("POST /v1/chat/completions "));
        let mut length = None;
        let mut authenticated = false;
        loop {
            line.clear();
            assert!(stream.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((header, value)) = line.split_once(':') {
                if header.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
                if header.eq_ignore_ascii_case("authorization") {
                    authenticated = value.trim() == "Bearer synthetic-key";
                }
            }
        }
        assert!(authenticated);
        let mut payload = vec![0; length.unwrap()];
        stream.read_exact(&mut payload).await.unwrap();
        let body: Value = serde_json::from_slice(&payload).unwrap();
        tokio::time::sleep(delay).await;
        let response = json!({
            "id": "synthetic-completion", "object": "chat.completion", "created": 1, "model": "test-model",
            "choices": [{"index":0, "message":{"role":"assistant", "content":answer}, "finish_reason":"stop"}],
            "usage": {"prompt_tokens": 21, "completion_tokens": 12, "total_tokens": 33},
        }).to_string();
        let message = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        );
        let sent = stream.get_mut().write_all(message.as_bytes()).await;
        if delay.is_zero() {
            sent.unwrap();
        }
        body
    });
    (endpoint, handle)
}
