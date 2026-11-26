//! Core-side ownership of the independent Agent executor process.

use std::path::PathBuf;

use dd_protocol::v1::{
    AgentCapabilities, AgentCapabilitiesRequest, HealthRequest, HealthSnapshot, ShutdownRequest,
    agent_service_client::AgentServiceClient,
};
use tokio::{process::Command, time::timeout};
use tonic::transport::Channel;
use uuid::Uuid;

pub use crate::local_process::LocalProcessError as AgentProcessError;
use crate::local_process::{LocalProcess, STARTUP_DEADLINE};

pub struct AgentProcess {
    process: LocalProcess,
    client: AgentServiceClient<Channel>,
}

impl AgentProcess {
    pub async fn launch(
        executable: PathBuf,
        executor_session: Uuid,
    ) -> Result<Self, AgentProcessError> {
        if !executable.is_absolute() {
            return Err(AgentProcessError::InvalidLaunch);
        }
        timeout(STARTUP_DEADLINE, async {
            let (process, channel) =
                LocalProcess::bootstrap(Command::new(executable), executor_session, None).await?;
            let mut agent = Self {
                process,
                client: AgentServiceClient::new(channel),
            };
            let health = agent.health().await?;
            LocalProcess::verify_identity(&health, "AGENT", executor_session)?;
            Ok(agent)
        })
        .await
        .map_err(|_| AgentProcessError::Deadline("startup"))?
    }

    pub async fn health(&mut self) -> Result<HealthSnapshot, AgentProcessError> {
        self.client
            .health(self.process.request(HealthRequest {}))
            .await
            .map(|response| response.into_inner())
            .map_err(AgentProcessError::Rpc)
    }

    pub async fn capabilities(&mut self) -> Result<AgentCapabilities, AgentProcessError> {
        self.client
            .get_capabilities(self.process.request(AgentCapabilitiesRequest {}))
            .await
            .map(|response| response.into_inner())
            .map_err(AgentProcessError::Rpc)
    }

    pub async fn shutdown(mut self) -> Result<(), AgentProcessError> {
        let receipt = self
            .client
            .shutdown(self.process.request(ShutdownRequest {}))
            .await
            .map_err(AgentProcessError::Rpc)?
            .into_inner();
        if !receipt.accepted {
            return Err(AgentProcessError::Shutdown);
        }
        self.process.wait_for_exit().await
    }
}
