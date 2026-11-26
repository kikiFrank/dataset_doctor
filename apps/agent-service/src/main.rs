use std::error::Error;
use std::io::{self, BufRead, Read, Write};
use std::net::Ipv4Addr;
use std::sync::Arc;

use dd_domain::workflow_templates::REGISTERED;
use dd_protocol::PROTOCOL_MAJOR;
use dd_protocol::v1::{
    AgentCapabilities, AgentCapabilitiesRequest, AgentTemplateCapability, HealthRequest,
    HealthSnapshot, ShutdownReceipt, ShutdownRequest,
    agent_service_server::{AgentService, AgentServiceServer},
};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Notify};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, transport::Server};
use uuid::Uuid;

const BOOTSTRAP_LIMIT: u64 = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapFrame {
    token: String,
    executor_session_id: String,
    protocol_major: u32,
}

struct Bootstrap {
    authorization: String,
    executor: Uuid,
}

fn read_bootstrap() -> io::Result<Bootstrap> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid Agent startup frame");
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(BOOTSTRAP_LIMIT + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > BOOTSTRAP_LIMIT || !bytes.ends_with(b"\n") {
        return Err(invalid());
    }
    let frame: BootstrapFrame = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let token = Uuid::parse_str(&frame.token).map_err(|_| invalid())?;
    let executor = Uuid::parse_str(&frame.executor_session_id).map_err(|_| invalid())?;
    if frame.protocol_major != PROTOCOL_MAJOR
        || token.is_nil()
        || frame.token != token.simple().to_string()
        || executor.is_nil()
        || frame.executor_session_id != executor.to_string()
    {
        return Err(invalid());
    }
    Ok(Bootstrap {
        authorization: format!("Bearer {}", frame.token),
        executor,
    })
}

struct AgentControl {
    executor: Uuid,
    shutdown: Arc<Notify>,
}

#[tonic::async_trait]
impl AgentService for AgentControl {
    async fn get_capabilities(
        &self,
        _request: Request<AgentCapabilitiesRequest>,
    ) -> Result<Response<AgentCapabilities>, Status> {
        let templates = REGISTERED
            .iter()
            .map(|template| AgentTemplateCapability {
                template_id: template.id.into(),
                contract_version: u32::from(template.version),
                execution_available: false,
            })
            .collect();
        Ok(Response::new(AgentCapabilities {
            protocol_major: PROTOCOL_MAJOR,
            templates,
            model_configured: false,
        }))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthSnapshot>, Status> {
        Ok(Response::new(HealthSnapshot {
            role: "AGENT".into(),
            executor_session_id: self.executor.to_string(),
            protocol_major: PROTOCOL_MAJOR,
            build_version: env!("CARGO_PKG_VERSION").into(),
            loaded_capability_ids: vec!["agent_lifecycle.v1".into()],
            active_executions: 0,
        }))
    }

    async fn shutdown(
        &self,
        _request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownReceipt>, Status> {
        self.shutdown.notify_one();
        Ok(Response::new(ShutdownReceipt { accepted: true }))
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let bootstrap = read_bootstrap()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(serve(bootstrap))
}

async fn serve(bootstrap: Bootstrap) -> Result<(), Box<dyn Error + Send + Sync>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    {
        let mut ready = io::stdout().lock();
        writeln!(ready, "{{\"port\":{}}}", listener.local_addr()?.port())?;
        ready.flush()?;
    }
    let shutdown = Arc::new(Notify::new());
    let service = AgentControl {
        executor: bootstrap.executor,
        shutdown: Arc::clone(&shutdown),
    };
    let authorization = bootstrap.authorization;
    let guarded = AgentServiceServer::with_interceptor(service, move |request: Request<()>| {
        let all = request.metadata().get_all("authorization");
        let mut values = all.iter();
        let accepted = values.next().is_some_and(|token| {
            bool::from(token.as_encoded_bytes().ct_eq(authorization.as_bytes()))
        });
        if !accepted || values.next().is_some() {
            return Err(Status::unauthenticated("Invalid Agent service credential"));
        }
        Ok(request)
    });
    Server::builder()
        .add_service(guarded)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
            shutdown.notified().await;
        })
        .await?;
    Ok(())
}
