//! The database owner thread for Workspace lifecycle requests.

use std::error::Error;
use std::fmt::{self, Display};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{ClaimReceipt, GrantScope};
use dd_core::workspace::{
    WorkspaceAccess, WorkspaceAccessError, WorkspaceDirectoryGrant, WorkspaceSession,
};
use uuid::Uuid;

use crate::task_store::{StoredTask, TaskStoreError, TaskSubmission};
use crate::workspace::{OpenedWorkspace, SqliteWorkspaceStore, WorkspaceOpenError};

const QUEUE_CAPACITY: usize = 8;
type AccessError = WorkspaceAccessError<WorkspaceOpenError>;
type AccessReply<T> = Sender<Result<T, AccessError>>;
type TaskAction = Box<dyn FnOnce(&mut WorkspaceAccess<SqliteWorkspaceStore>) + Send>;

#[derive(Debug)]
pub enum WorkspaceThreadError {
    Startup(Box<dyn Error + Send + Sync>),
    Access(AccessError),
    Task(TaskStoreError),
    Busy,
    Stopped,
}

impl Display for WorkspaceThreadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => write!(formatter, "Workspace thread startup: {error}"),
            Self::Access(error) => Display::fmt(error, formatter),
            Self::Task(error) => Display::fmt(error, formatter),
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
            Self::Task(error) => Some(error),
            _ => None,
        }
    }
}

enum Request {
    Grant(PathBuf, AccessReply<WorkspaceDirectoryGrant>),
    Open(Uuid, AccessReply<WorkspaceSession>),
    Create(Uuid, String, i64, AccessReply<WorkspaceSession>),
    Current(Sender<Option<WorkspaceSession>>),
    Close(Uuid, AccessReply<()>),
    Task(TaskAction),
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
            SqliteWorkspaceStore::new(busy_timeout_ms).map_err(WorkspaceThreadError::Startup)?;
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
                        Request::Create(id, name, now_ms, reply) => {
                            let _ = reply.send(access.create(id, &name, Instant::now(), now_ms));
                        }
                        Request::Current(reply) => {
                            let _ = reply.send(access.current().cloned());
                        }
                        Request::Close(id, reply) => {
                            let _ = reply.send(access.close(id));
                        }
                        Request::Task(action) => action(&mut access),
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

    pub fn create(
        &self,
        grant_id: Uuid,
        display_name: String,
        now_ms: i64,
    ) -> Result<WorkspaceSession, WorkspaceThreadError> {
        self.request(|reply| Request::Create(grant_id, display_name, now_ms, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    pub fn current(&self) -> Result<Option<WorkspaceSession>, WorkspaceThreadError> {
        self.request(Request::Current)
    }

    pub fn close(&self, expected_session_id: Uuid) -> Result<(), WorkspaceThreadError> {
        self.request(|reply| Request::Close(expected_session_id, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    pub fn enqueue_task(
        &self,
        task: TaskSubmission,
        now_ms: i64,
    ) -> Result<StoredTask, WorkspaceThreadError> {
        self.task(task.workspace_session, move |workspace| {
            workspace.enqueue_task(&task, now_ms)
        })
    }

    pub fn record_dispatch(
        &self,
        scope: GrantScope,
        grant: Uuid,
        node_spec_fingerprint: [u8; 32],
        now_ms: i64,
    ) -> Result<(), WorkspaceThreadError> {
        self.task(scope.workspace_session, move |workspace| {
            workspace.record_dispatch(&scope, grant, node_spec_fingerprint, now_ms)
        })
    }

    pub fn record_claimed(
        &self,
        session: Uuid,
        grant: Uuid,
        receipt: ClaimReceipt,
        now_ms: i64,
    ) -> Result<(), WorkspaceThreadError> {
        self.task(session, move |workspace| {
            workspace.record_claimed(session, grant, receipt, now_ms)
        })
    }

    fn task<T: Send + 'static>(
        &self,
        session: Uuid,
        execute: impl FnOnce(&mut OpenedWorkspace) -> Result<T, TaskStoreError> + Send + 'static,
    ) -> Result<T, WorkspaceThreadError> {
        self.request(|reply| {
            Request::Task(Box::new(move |access| {
                let result = access
                    .active_handle(session)
                    .map_err(WorkspaceThreadError::Access)
                    .and_then(|workspace| execute(workspace).map_err(WorkspaceThreadError::Task));
                let _ = reply.send(result);
            }))
        })?
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
