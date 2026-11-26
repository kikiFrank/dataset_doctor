//! The database owner thread for Workspace lifecycle requests.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dd_core::workspace::{
    WorkspaceAccess, WorkspaceAccessError, WorkspaceDirectoryGrant, WorkspaceSession,
};
use uuid::Uuid;

use crate::workspace::{ExistingWorkspaceStore, WorkspaceOpenError};

const QUEUE_CAPACITY: usize = 8;
type AccessError = WorkspaceAccessError<WorkspaceOpenError>;
type AccessReply<T> = Sender<Result<T, AccessError>>;

#[derive(Debug)]
pub enum WorkspaceThreadError {
    Startup(Box<dyn Error + Send + Sync>),
    Access(AccessError),
    Busy,
    Stopped,
}

impl Display for WorkspaceThreadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => write!(formatter, "Workspace thread startup: {error}"),
            Self::Access(error) => Display::fmt(error, formatter),
            Self::Busy => write!(formatter, "Workspace request queue is full"),
            Self::Stopped => write!(formatter, "Workspace thread has stopped"),
        }
    }
}

impl Error for WorkspaceThreadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Startup(error) => Some(error.as_ref()),
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

enum Request {
    Grant(PathBuf, AccessReply<WorkspaceDirectoryGrant>),
    Open(Uuid, AccessReply<WorkspaceSession>),
    Current(Sender<Option<WorkspaceSession>>),
    Close(Uuid, AccessReply<()>),
}

pub struct WorkspaceThread {
    requests: Option<SyncSender<Request>>,
    owner: Option<JoinHandle<()>>,
}

impl WorkspaceThread {
    pub fn start(
        busy_timeout_ms: u32,
        grant_lifetime: Duration,
    ) -> Result<Self, WorkspaceThreadError> {
        let store =
            ExistingWorkspaceStore::new(busy_timeout_ms).map_err(WorkspaceThreadError::Startup)?;
        let (requests, inbox) = mpsc::sync_channel(QUEUE_CAPACITY);
        let owner = thread::Builder::new()
            .name("dd-workspace-db".into())
            .spawn(move || {
                let mut access = WorkspaceAccess::new(store, grant_lifetime);
                for request in inbox {
                    match request {
                        Request::Grant(path, reply) => {
                            let _ =
                                reply.send(access.grant_selected_directory(path, Instant::now()));
                        }
                        Request::Open(id, reply) => {
                            let _ = reply.send(access.open(id, Instant::now()));
                        }
                        Request::Current(reply) => {
                            let _ = reply.send(access.current().cloned());
                        }
                        Request::Close(id, reply) => {
                            let _ = reply.send(access.close(id));
                        }
                    }
                }
            })
            .map_err(|error| WorkspaceThreadError::Startup(Box::new(error)))?;
        Ok(Self {
            requests: Some(requests),
            owner: Some(owner),
        })
    }

    pub fn grant_selected_directory(
        &self,
        selected: PathBuf,
    ) -> Result<WorkspaceDirectoryGrant, WorkspaceThreadError> {
        self.request(|reply| Request::Grant(selected, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    pub fn open(&self, grant_id: Uuid) -> Result<WorkspaceSession, WorkspaceThreadError> {
        self.request(|reply| Request::Open(grant_id, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    pub fn current(&self) -> Result<Option<WorkspaceSession>, WorkspaceThreadError> {
        self.request(Request::Current)
    }

    pub fn close(&self, expected_session_id: Uuid) -> Result<(), WorkspaceThreadError> {
        self.request(|reply| Request::Close(expected_session_id, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    fn request<T>(
        &self,
        message: impl FnOnce(Sender<T>) -> Request,
    ) -> Result<T, WorkspaceThreadError> {
        let (reply, result) = mpsc::channel();
        let sender = self
            .requests
            .as_ref()
            .ok_or(WorkspaceThreadError::Stopped)?;
        sender
            .try_send(message(reply))
            .map_err(|error| match error {
                TrySendError::Full(_) => WorkspaceThreadError::Busy,
                TrySendError::Disconnected(_) => WorkspaceThreadError::Stopped,
            })?;
        result.recv().map_err(|_| WorkspaceThreadError::Stopped)
    }
}

impl Drop for WorkspaceThread {
    fn drop(&mut self) {
        drop(self.requests.take());
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}
