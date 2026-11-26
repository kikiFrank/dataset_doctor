//! Start and supervise the local Python Worker without shell interpolation.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use dd_protocol::PROTOCOL_MAJOR;
use dd_protocol::v1::{
    ExecutionHandle, ExecutionLookup, ExecutionSnapshot, GeometryAuditRequest, HealthRequest,
    HealthSnapshot, ShutdownRequest, python_worker_client::PythonWorkerClient,
};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};
use uuid::Uuid;

const STARTUP_DEADLINE: Duration = Duration::from_secs(10);
const RPC_DEADLINE: Duration = Duration::from_secs(10);
const READY_LIMIT: u64 = 4096;

pub struct CoreClaimEndpoint {
    pub address: SocketAddrV4,
    pub claim_token: Uuid,
}

pub struct WorkerLaunch {
    pub interpreter: PathBuf,
    pub entrypoint: PathBuf,
    pub executor_session: Uuid,
    pub core: Option<CoreClaimEndpoint>,
}

#[derive(Debug)]
pub enum WorkerProcessError {
    InvalidLaunch,
    InvalidReadyFrame,
    InvalidHandshake,
    Deadline(&'static str),
    Shutdown,
    Io(io::Error),
    Transport(tonic::transport::Error),
    Rpc(Status),
}

impl Display for WorkerProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLaunch => {
                formatter.write_str("Worker launch needs local absolute paths and loopback control")
            }
            Self::InvalidReadyFrame => formatter.write_str("Invalid Worker startup frame"),
            Self::InvalidHandshake => {
                formatter.write_str("Worker process identity or protocol does not match its launch")
            }
            Self::Deadline(operation) => write!(formatter, "Worker {operation} timed out"),
            Self::Shutdown => formatter.write_str("Worker did not stop cleanly"),
            Self::Io(error) => write!(formatter, "Supervise Worker process: {error}"),
            Self::Transport(error) => write!(formatter, "Connect to Worker loopback: {error}"),
            Self::Rpc(error) => write!(formatter, "Worker RPC: {}", error.code()),
        }
    }
}

impl Error for WorkerProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Rpc(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for WorkerProcessError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    port: u16,
}

pub struct WorkerProcess {
    child: Child,
    client: PythonWorkerClient<Channel>,
    authorization: MetadataValue<Ascii>,
}

impl WorkerProcess {
    pub async fn launch(config: WorkerLaunch) -> Result<Self, WorkerProcessError> {
        timeout(STARTUP_DEADLINE, Self::bootstrap(config))
            .await
            .map_err(|_| WorkerProcessError::Deadline("startup"))?
    }

    async fn bootstrap(config: WorkerLaunch) -> Result<Self, WorkerProcessError> {
        if !config.interpreter.is_absolute()
            || !config.entrypoint.is_absolute()
            || config.core.as_ref().is_some_and(|core| {
                *core.address.ip() != Ipv4Addr::LOCALHOST || core.address.port() == 0
            })
        {
            return Err(WorkerProcessError::InvalidLaunch);
        }
        let token = Uuid::new_v4().simple().to_string();
        let expected_session = config.executor_session.to_string();
        let mut bootstrap = json!({
            "token": token,
            "executor_session_id": expected_session,
            "protocol_major": PROTOCOL_MAJOR,
        });
        if let Some(core) = config.core {
            bootstrap["core"] = json!({"address": core.address.to_string(), "claim_token": core.claim_token.simple().to_string()});
        }
        let mut command = Command::new(config.interpreter);
        command
            .args(["-I", "-B"])
            .arg(config.entrypoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = command.spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or(WorkerProcessError::InvalidReadyFrame)?;
        input.write_all(format!("{bootstrap}\n").as_bytes()).await?;
        input.shutdown().await?;
        drop(input);

        let output = child
            .stdout
            .take()
            .ok_or(WorkerProcessError::InvalidReadyFrame)?;
        let mut raw = Vec::new();
        BufReader::new(output)
            .take(READY_LIMIT + 1)
            .read_until(b'\n', &mut raw)
            .await?;
        if raw.len() as u64 > READY_LIMIT || !raw.ends_with(b"\n") {
            return Err(WorkerProcessError::InvalidReadyFrame);
        }
        let ready: Ready =
            serde_json::from_slice(&raw).map_err(|_| WorkerProcessError::InvalidReadyFrame)?;
        if ready.port == 0 {
            return Err(WorkerProcessError::InvalidReadyFrame);
        }
        let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{}", ready.port))
            .map_err(WorkerProcessError::Transport)?;
        let client = PythonWorkerClient::connect(endpoint)
            .await
            .map_err(WorkerProcessError::Transport)?;
        let mut authorization: MetadataValue<Ascii> = format!("Bearer {token}")
            .parse()
            .expect("UUID bearer token is ASCII");
        authorization.set_sensitive(true);
        let mut worker = Self {
            child,
            client,
            authorization,
        };
        let health = worker.health().await?;
        if health.role != "WORKER"
            || health.executor_session_id != expected_session
            || health.protocol_major != PROTOCOL_MAJOR
        {
            return Err(WorkerProcessError::InvalidHandshake);
        }
        Ok(worker)
    }

    pub async fn health(&mut self) -> Result<HealthSnapshot, WorkerProcessError> {
        let request = self.request(HealthRequest {});
        self.client
            .health(request)
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn start_geometry(
        &mut self,
        request: GeometryAuditRequest,
    ) -> Result<ExecutionHandle, WorkerProcessError> {
        let request = self.request(request);
        self.client
            .start_annotation_geometry_audit(request)
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn execution(
        &mut self,
        lookup: ExecutionLookup,
    ) -> Result<ExecutionSnapshot, WorkerProcessError> {
        let request = self.request(lookup);
        self.client
            .get_execution(request)
            .await
            .map(|reply| reply.into_inner())
            .map_err(WorkerProcessError::Rpc)
    }

    pub async fn shutdown(mut self) -> Result<(), WorkerProcessError> {
        let request = self.request(ShutdownRequest {});
        if !self
            .client
            .shutdown(request)
            .await
            .map_err(WorkerProcessError::Rpc)?
            .into_inner()
            .accepted
        {
            return Err(WorkerProcessError::Shutdown);
        }
        let status = timeout(RPC_DEADLINE, self.child.wait())
            .await
            .map_err(|_| WorkerProcessError::Deadline("shutdown"))??;
        if !status.success() {
            return Err(WorkerProcessError::Shutdown);
        }
        Ok(())
    }

    fn request<T>(&self, content: T) -> Request<T> {
        let mut request = Request::new(content);
        request.set_timeout(RPC_DEADLINE);
        request
            .metadata_mut()
            .insert("authorization", self.authorization.clone());
        request
    }
}
