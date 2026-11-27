//! Bootstrap and supervise the local executor processes.

#[cfg(windows)]
mod windows_job;

use std::error::Error;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::process::Stdio;
use std::time::Duration;

use dd_protocol::PROTOCOL_MAJOR;
use dd_protocol::v1::HealthSnapshot;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};
use uuid::Uuid;

pub(super) const STARTUP_DEADLINE: Duration = Duration::from_secs(10);
const RPC_DEADLINE: Duration = Duration::from_secs(10);
const READY_LIMIT: u64 = 4096;

pub struct CoreClaimEndpoint {
    pub address: SocketAddrV4,
    pub claim_token: Uuid,
}

#[derive(Debug)]
pub enum LocalProcessError {
    InvalidLaunch,
    InvalidReadyFrame,
    InvalidHandshake,
    Deadline(&'static str),
    Shutdown,
    Io(io::Error),
    Transport(tonic::transport::Error),
    Rpc(Status),
}

impl fmt::Display for LocalProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLaunch => formatter
                .write_str("Executor launch requires absolute local paths and loopback control"),
            Self::InvalidReadyFrame => formatter.write_str("Invalid executor startup frame"),
            Self::InvalidHandshake => {
                formatter.write_str("Executor identity or protocol does not match its launch")
            }
            Self::Deadline(operation) => write!(formatter, "Executor {operation} timed out"),
            Self::Shutdown => formatter.write_str("The executor did not stop cleanly"),
            Self::Io(error) => write!(formatter, "Supervise executor process: {error}"),
            Self::Transport(error) => write!(formatter, "Connect to executor loopback: {error}"),
            Self::Rpc(error) => write!(formatter, "Executor RPC: {}", error.code()),
        }
    }
}

impl Error for LocalProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Rpc(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for LocalProcessError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    port: u16,
}

pub(super) struct LocalProcess {
    child: Child,
    #[cfg(windows)]
    job: windows_job::ProcessJob,
    authorization: MetadataValue<Ascii>,
}

impl LocalProcess {
    pub(super) async fn bootstrap(
        mut command: Command,
        executor_session: Uuid,
        core: Option<CoreClaimEndpoint>,
    ) -> Result<(Self, Channel), LocalProcessError> {
        if executor_session.is_nil()
            || core.as_ref().is_some_and(|core| {
                *core.address.ip() != Ipv4Addr::LOCALHOST || core.address.port() == 0
            })
        {
            return Err(LocalProcessError::InvalidLaunch);
        }
        let token = Uuid::new_v4().simple().to_string();
        let mut frame = json!({
            "token": token,
            "executor_session_id": executor_session,
            "protocol_major": PROTOCOL_MAJOR,
        });
        if let Some(core) = core {
            frame["core"] = json!({"address": core.address.to_string(), "claim_token": core.claim_token.simple().to_string()});
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = windows_job::ProcessJob::attach(&child)?;
        let mut input = child
            .stdin
            .take()
            .ok_or(LocalProcessError::InvalidReadyFrame)?;
        input.write_all(format!("{frame}\n").as_bytes()).await?;
        input.shutdown().await?;
        drop(input);

        let output = child
            .stdout
            .take()
            .ok_or(LocalProcessError::InvalidReadyFrame)?;
        let mut bytes = Vec::new();
        BufReader::new(output)
            .take(READY_LIMIT + 1)
            .read_until(b'\n', &mut bytes)
            .await?;
        if bytes.len() as u64 > READY_LIMIT || !bytes.ends_with(b"\n") {
            return Err(LocalProcessError::InvalidReadyFrame);
        }
        let ready: Ready =
            serde_json::from_slice(&bytes).map_err(|_| LocalProcessError::InvalidReadyFrame)?;
        if ready.port == 0 {
            return Err(LocalProcessError::InvalidReadyFrame);
        }
        let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{}", ready.port))
            .map_err(LocalProcessError::Transport)?;
        let channel = endpoint
            .connect()
            .await
            .map_err(LocalProcessError::Transport)?;
        let mut authorization: MetadataValue<Ascii> = format!("Bearer {token}")
            .parse()
            .expect("UUID bearer token is ASCII");
        authorization.set_sensitive(true);
        Ok((
            Self {
                child,
                #[cfg(windows)]
                job,
                authorization,
            },
            channel,
        ))
    }

    pub(super) fn verify_identity(
        health: &HealthSnapshot,
        role: &str,
        session: Uuid,
    ) -> Result<(), LocalProcessError> {
        if health.role != role
            || health.executor_session_id != session.to_string()
            || health.protocol_major != PROTOCOL_MAJOR
        {
            return Err(LocalProcessError::InvalidHandshake);
        }
        Ok(())
    }

    pub(super) fn request<T>(&self, body: T) -> Request<T> {
        let mut request = Request::new(body);
        request.set_timeout(RPC_DEADLINE);
        request
            .metadata_mut()
            .insert("authorization", self.authorization.clone());
        request
    }

    pub(super) async fn wait_for_exit(mut self) -> Result<(), LocalProcessError> {
        let status = timeout(RPC_DEADLINE, self.child.wait())
            .await
            .map_err(|_| LocalProcessError::Deadline("shutdown"))??;
        #[cfg(windows)]
        self.job.stop().await?;
        if !status.success() {
            return Err(LocalProcessError::Shutdown);
        }
        Ok(())
    }

    pub(super) async fn terminate(mut self) -> Result<(), LocalProcessError> {
        #[cfg(windows)]
        self.job.stop().await?;
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        if let Err(error) = self.child.start_kill() {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            return Err(error.into());
        }
        timeout(RPC_DEADLINE, self.child.wait())
            .await
            .map_err(|_| LocalProcessError::Deadline("termination"))??;
        Ok(())
    }
}
