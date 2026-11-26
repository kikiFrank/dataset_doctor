use std::error::Error;
use std::fmt::{self, Display};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSession {
    pub workspace_id: String,
    pub display_name: String,
    pub workspace_session_id: Uuid,
    pub config_revision: u64,
    pub offline_asset_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceIdentity {
    pub workspace_id: String,
    pub display_name: String,
    pub config_revision: u64,
    pub offline_asset_count: u64,
}

pub trait WorkspaceStore {
    type Handle;
    type Error: Error;

    fn open(
        &self,
        authorized_root: &Path,
    ) -> Result<(WorkspaceIdentity, Self::Handle), Self::Error>;

    fn create(
        &self,
        authorized_root: &Path,
        display_name: &str,
        workspace_id: Uuid,
        now_ms: i64,
    ) -> Result<(WorkspaceIdentity, Self::Handle), Self::Error>;
}

#[derive(Debug)]
pub enum WorkspaceAccessError<E> {
    InvalidSelection,
    NotAuthorized,
    ExpiredGrant,
    AlreadyOpen,
    Closed,
    StaleSession,
    Storage(E),
}

impl<E: Display> Display for WorkspaceAccessError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSelection => write!(
                formatter,
                "Workspace selection must be an absolute directory"
            ),
            Self::NotAuthorized => write!(formatter, "Workspace directory grant is unknown"),
            Self::ExpiredGrant => write!(formatter, "Workspace directory grant expired"),
            Self::AlreadyOpen => write!(
                formatter,
                "Close the current Workspace before opening another"
            ),
            Self::Closed => write!(formatter, "No Workspace is open"),
            Self::StaleSession => write!(formatter, "Workspace session has changed"),
            Self::Storage(error) => Display::fmt(error, formatter),
        }
    }
}

impl<E: Error + 'static> Error for WorkspaceAccessError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct WorkspaceDirectoryGrant {
    pub grant_id: Uuid,
    pub display_path: PathBuf,
}

struct PendingGrant {
    id: Uuid,
    root: PathBuf,
    issued_at: Instant,
}

struct ActiveWorkspace<H> {
    session: WorkspaceSession,
    handle: H,
}

pub struct WorkspaceAccess<S: WorkspaceStore> {
    store: S,
    grant_lifetime: Duration,
    pending: Option<PendingGrant>,
    active: Option<ActiveWorkspace<S::Handle>>,
}

impl<S: WorkspaceStore> WorkspaceAccess<S> {
    pub fn new(store: S, grant_lifetime: Duration) -> Self {
        Self {
            store,
            grant_lifetime,
            pending: None,
            active: None,
        }
    }

    /// Accept a directory returned by the trusted native selector. The
    /// transport layer exposes only the opaque ID for Workspace open or create.
    pub fn grant_selected_directory(
        &mut self,
        selected: PathBuf,
        now: Instant,
    ) -> Result<WorkspaceDirectoryGrant, WorkspaceAccessError<S::Error>> {
        if !selected.is_absolute() {
            return Err(WorkspaceAccessError::InvalidSelection);
        }
        let grant_id = Uuid::new_v4();
        self.pending = Some(PendingGrant {
            id: grant_id,
            root: selected.clone(),
            issued_at: now,
        });
        Ok(WorkspaceDirectoryGrant {
            grant_id,
            display_path: selected,
        })
    }

    pub fn open(
        &mut self,
        grant_id: Uuid,
        now: Instant,
    ) -> Result<WorkspaceSession, WorkspaceAccessError<S::Error>> {
        let root = self.consume_directory_grant(grant_id, now)?;
        let (identity, handle) = self
            .store
            .open(&root)
            .map_err(WorkspaceAccessError::Storage)?;
        Ok(self.activate(identity, handle))
    }

    pub fn create(
        &mut self,
        grant_id: Uuid,
        display_name: &str,
        now: Instant,
        now_ms: i64,
    ) -> Result<WorkspaceSession, WorkspaceAccessError<S::Error>> {
        let root = self.consume_directory_grant(grant_id, now)?;
        let (identity, handle) = self
            .store
            .create(&root, display_name, Uuid::new_v4(), now_ms)
            .map_err(WorkspaceAccessError::Storage)?;
        Ok(self.activate(identity, handle))
    }

    fn consume_directory_grant(
        &mut self,
        grant_id: Uuid,
        now: Instant,
    ) -> Result<PathBuf, WorkspaceAccessError<S::Error>> {
        if self.active.is_some() {
            return Err(WorkspaceAccessError::AlreadyOpen);
        }
        if self.pending.as_ref().map(|grant| grant.id) != Some(grant_id) {
            return Err(WorkspaceAccessError::NotAuthorized);
        }
        let grant = self
            .pending
            .take()
            .ok_or(WorkspaceAccessError::NotAuthorized)?;
        if now.saturating_duration_since(grant.issued_at) >= self.grant_lifetime {
            return Err(WorkspaceAccessError::ExpiredGrant);
        }
        Ok(grant.root)
    }

    fn activate(&mut self, identity: WorkspaceIdentity, handle: S::Handle) -> WorkspaceSession {
        let session = WorkspaceSession {
            workspace_id: identity.workspace_id,
            display_name: identity.display_name,
            workspace_session_id: Uuid::new_v4(),
            config_revision: identity.config_revision,
            offline_asset_count: identity.offline_asset_count,
        };
        self.active = Some(ActiveWorkspace {
            session: session.clone(),
            handle,
        });
        session
    }

    pub fn current(&self) -> Option<&WorkspaceSession> {
        self.active.as_ref().map(|active| &active.session)
    }

    pub fn active_handle(
        &mut self,
        session: Uuid,
    ) -> Result<&mut S::Handle, WorkspaceAccessError<S::Error>> {
        let active = self.active.as_mut().ok_or(WorkspaceAccessError::Closed)?;
        if active.session.workspace_session_id != session {
            return Err(WorkspaceAccessError::StaleSession);
        }
        Ok(&mut active.handle)
    }

    pub fn close(
        &mut self,
        expected_session_id: Uuid,
    ) -> Result<(), WorkspaceAccessError<S::Error>> {
        let current = self.current().ok_or(WorkspaceAccessError::Closed)?;
        if current.workspace_session_id != expected_session_id {
            return Err(WorkspaceAccessError::StaleSession);
        }
        self.active = None;
        Ok(())
    }
}
