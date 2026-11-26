use std::path::PathBuf;

use dd_infra::agent_process::{AgentProcess, AgentProcessError};
use uuid::Uuid;

#[tokio::test]
async fn core_manages_the_real_agent_process_and_can_start_a_fresh_session() {
    let executable = PathBuf::from(
        std::env::var_os("DD_AGENT_PROCESS_EXE").expect("run Agent supervisor acceptance"),
    );
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    assert!(matches!(
        AgentProcess::launch(PathBuf::from("dd-agent-service.exe"), first).await,
        Err(AgentProcessError::InvalidLaunch)
    ));
    let mut agent = AgentProcess::launch(executable.clone(), first)
        .await
        .unwrap();
    let health = agent.health().await.unwrap();
    assert_eq!(health.role, "AGENT");
    assert_eq!(health.executor_session_id, first.to_string());
    assert_eq!(health.protocol_major, dd_protocol::PROTOCOL_MAJOR);
    let capabilities = agent.capabilities().await.unwrap();
    assert_eq!(capabilities.templates.len(), 3);
    assert!(!capabilities.model_configured);
    agent.shutdown().await.unwrap();
    let mut replacement = AgentProcess::launch(executable, second).await.unwrap();
    let health = replacement.health().await.unwrap();
    assert_eq!(health.executor_session_id, second.to_string());
    replacement.shutdown().await.unwrap();
}
