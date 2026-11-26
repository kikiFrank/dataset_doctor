//! Start and supervise the local Python Worker without shell interpolation.

use std::path::PathBuf;

use dd_protocol::v1::{
    ColorPreviewRequest, ExecutionHandle, ExecutionLookup, ExecutionSnapshot, GeometryAuditRequest,
    HealthRequest, HealthSnapshot, ShutdownRequest, python_worker_client::PythonWorkerClient,
};
use tokio::{process::Command, time::timeout};
use tonic::transport::Channel;
use uuid::Uuid;

pub use crate::local_process::{CoreClaimEndpoint, LocalProcessError as WorkerProcessError};
use crate::local_process::{LocalProcess, STARTUP_DEADLINE};

pub struct WorkerLaunch {
    pub interpreter: PathBuf,
    pub entrypoint: PathBuf,
    pub executor_session: Uuid,
    pub core: Option<CoreClaimEndpoint>,
}

pub struct WorkerProcess {
    process: LocalProcess,
    client: PythonWorkerClient<Channel>,
}

impl WorkerProcess {
    pub async fn launch(config: WorkerLaunch) -> Result<Self, WorkerProcessError> {
        if !config.interpreter.is_absolute() || !config.entrypoint.is_absolute() {
            return Err(WorkerProcessError::InvalidLaunch);
        }
        timeout(STARTUP_DEADLINE, async {
            let mut command = Command::new(config.interpreter);
            command.args(["-I", "-B"]).arg(config.entrypoint);
            let (process, channel) =
                LocalProcess::bootstrap(command, config.executor_session, config.core).await?;
            let mut worker = Self {
                process,
                client: PythonWorkerClient::new(channel),
            };
            let health = worker.health().await?;
            LocalProcess::verify_identity(&health, "WORKER", config.executor_session)?;
            Ok(worker)
        })
        .await
        .map_err(|_| WorkerProcessError::Deadline("startup"))?
    }

    pub async fn health(&mut self) -> Result<HealthSnapshot, WorkerProcessError> {
        self.client
            .health(self.process.request(HealthRequest {}))
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn start_geometry(
        &mut self,
        request: GeometryAuditRequest,
    ) -> Result<ExecutionHandle, WorkerProcessError> {
        self.client
            .start_annotation_geometry_audit(self.process.request(request))
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn start_color_preview(
        &mut self,
        request: ColorPreviewRequest,
    ) -> Result<ExecutionHandle, WorkerProcessError> {
        self.client
            .start_color_preview(self.process.request(request))
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn execution(
        &mut self,
        lookup: ExecutionLookup,
    ) -> Result<ExecutionSnapshot, WorkerProcessError> {
        self.client
            .get_execution(self.process.request(lookup))
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn shutdown(mut self) -> Result<(), WorkerProcessError> {
        let receipt = self
            .client
            .shutdown(self.process.request(ShutdownRequest {}))
            .await
            .map_err(WorkerProcessError::Rpc)?
            .into_inner();
        if !receipt.accepted {
            return Err(WorkerProcessError::Shutdown);
        }
        self.process.wait_for_exit().await
    }

    pub(crate) async fn terminate(self) -> Result<(), WorkerProcessError> {
        self.process.terminate().await
    }
}
