//! The database owner thread for Workspace lifecycle requests.

use std::error::Error;
use std::fmt::{self, Display};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
#[cfg(feature = "execution-rpc")]
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{ClaimReceipt, GrantScope};
use dd_core::workspace::{
    WorkspaceAccess, WorkspaceAccessError, WorkspacePathGrant, WorkspacePathPurpose,
    WorkspaceSession,
};
use uuid::Uuid;

use crate::dataset_query::{
    AssetAnnotationState, AssetSummary, DatasetPage, DatasetQueryError, DatasetSummary,
    RegisteredImage,
};
use crate::import_registration::{ImportRegistration, ImportRegistrationError, PreparedImport};
use crate::paths::open_registered_file;
use crate::review_store::{ReviewBaseline, ReviewSession, ReviewStoreError};
#[cfg(feature = "execution-rpc")]
use crate::task_store::{ReportScope, ResultSubmission, TerminalReceipt};
use crate::task_store::{StoredTask, TaskOutcome, TaskStoreError, TaskSubmission};
use crate::workspace::{OpenedWorkspace, SqliteWorkspaceStore, WorkspaceOpenError, ordinary_file};

use crate::workspace_backup::{WorkspacePackageError, restore_workspace_backup};

const QUEUE_CAPACITY: usize = 8;
type AccessError = WorkspaceAccessError<WorkspaceOpenError>;
type AccessReply<T> = Sender<Result<T, AccessError>>;
type WorkspaceAction = Box<dyn FnOnce(&mut WorkspaceAccess<SqliteWorkspaceStore>) + Send>;

#[derive(Debug)]
pub enum WorkspaceThreadError {
    Startup(Box<dyn Error + Send + Sync>),
    Access(AccessError),
    Task(TaskStoreError),
    Import(ImportRegistrationError),
    Dataset(DatasetQueryError),
    Review(ReviewStoreError),
    WorkflowDatabase(rusqlite::Error),
    Backup(WorkspacePackageError),
    Busy,
    Stopped,
}

impl Display for WorkspaceThreadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => write!(formatter, "Workspace thread startup: {error}"),
            Self::Access(error) => Display::fmt(error, formatter),
            Self::Task(error) => Display::fmt(error, formatter),
            Self::Import(error) => Display::fmt(error, formatter),
            Self::Dataset(error) => Display::fmt(error, formatter),
            Self::Review(error) => Display::fmt(error, formatter),
            Self::WorkflowDatabase(error) => {
                write!(formatter, "Workspace workflow persistence: {error}")
            }
            Self::Backup(error) => Display::fmt(error, formatter),
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
            Self::Import(error) => Some(error),
            Self::Dataset(error) => Some(error),
            Self::Review(error) => Some(error),
            Self::WorkflowDatabase(error) => Some(error),
            Self::Backup(error) => Some(error),
            _ => None,
        }
    }
}

enum Request {
    Grant(PathBuf, AccessReply<WorkspacePathGrant>),
    GrantPath(
        Uuid,
        WorkspacePathPurpose,
        PathBuf,
        AccessReply<WorkspacePathGrant>,
    ),
    ConsumePath(Uuid, WorkspacePathPurpose, Uuid, AccessReply<PathBuf>),
    Open(Uuid, AccessReply<WorkspaceSession>),
    Restore(
        PathBuf,
        PathBuf,
        Sender<Result<WorkspaceSession, WorkspaceThreadError>>,
    ),
    Create(Uuid, String, i64, AccessReply<WorkspaceSession>),
    Current(Sender<Option<WorkspaceSession>>),
    Close(Uuid, AccessReply<()>),
    Action(WorkspaceAction),
}

pub struct WorkspaceThread {
    requests: Option<SyncSender<Request>>,
    owner: Option<JoinHandle<()>>,
    #[cfg(feature = "execution-rpc")]
    final_operations: Mutex<()>,
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
                        Request::GrantPath(session, purpose, path, reply) => {
                            let _ = reply.send(access.grant_for(
                                session,
                                purpose,
                                path,
                                Instant::now(),
                            ));
                        }
                        Request::ConsumePath(session, purpose, grant, reply) => {
                            let _ = reply.send(access.consume_for(
                                session,
                                purpose,
                                grant,
                                Instant::now(),
                            ));
                        }
                        Request::Open(id, reply) => {
                            let _ = reply.send(access.open(id, Instant::now()));
                        }
                        Request::Restore(package, destination, reply) => {
                            let result = (|| {
                                if access.current().is_some() {
                                    return Err(WorkspaceThreadError::Access(
                                        WorkspaceAccessError::AlreadyOpen,
                                    ));
                                }
                                restore_workspace_backup(&package, &destination)
                                    .map_err(WorkspaceThreadError::Backup)?;
                                let now = Instant::now();
                                let grant = access
                                    .grant_selected_directory(destination, now)
                                    .map_err(WorkspaceThreadError::Access)?;
                                access
                                    .open(grant.grant_id, now)
                                    .map_err(WorkspaceThreadError::Access)
                            })();
                            let _ = reply.send(result);
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
                        Request::Action(action) => action(&mut access),
                    }
                }
            })
            .map_err(|error| WorkspaceThreadError::Startup(Box::new(error)))?;
        Ok(Self {
            requests: Some(requests),
            owner: Some(owner),
            #[cfg(feature = "execution-rpc")]
            final_operations: Mutex::new(()),
        })
    }

    pub fn grant_selected_directory(
        &self,
        selected: PathBuf,
    ) -> Result<WorkspacePathGrant, WorkspaceThreadError> {
        self.request(|reply| Request::Grant(selected, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    pub fn open(&self, grant_id: Uuid) -> Result<WorkspaceSession, WorkspaceThreadError> {
        self.request(|reply| Request::Open(grant_id, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    /// Restore only from trusted local selections while the owner has no open Workspace.
    pub fn restore_backup(
        &self,
        package: PathBuf,
        destination: PathBuf,
    ) -> Result<WorkspaceSession, WorkspaceThreadError> {
        self.request(|reply| Request::Restore(package, destination, reply))?
    }

    pub fn grant_import_directory(
        &self,
        session: Uuid,
        selected: PathBuf,
    ) -> Result<WorkspacePathGrant, WorkspaceThreadError> {
        self.grant_path(session, WorkspacePathPurpose::Import, selected)
    }

    /// Grant one file selected by the trusted video chooser.
    pub fn grant_video_file(
        &self,
        session: Uuid,
        selected: PathBuf,
    ) -> Result<WorkspacePathGrant, WorkspaceThreadError> {
        let invalid = || WorkspaceThreadError::Access(WorkspaceAccessError::InvalidSelection);
        if !selected.is_absolute()
            || !fs::symlink_metadata(&selected).is_ok_and(|metadata| ordinary_file(&metadata))
        {
            return Err(invalid());
        }
        let selected = selected.canonicalize().map_err(|_| invalid())?;
        open_registered_file(&selected).map_err(|_| invalid())?;
        self.grant_path(session, WorkspacePathPurpose::VideoImport, selected)
    }

    #[cfg(feature = "execution-rpc")]
    pub fn grant_export_directory(
        &self,
        session: Uuid,
        selected: PathBuf,
    ) -> Result<WorkspacePathGrant, WorkspaceThreadError> {
        let selected = selected
            .canonicalize()
            .ok()
            .filter(|path| path.is_dir())
            .ok_or(WorkspaceThreadError::Access(
                WorkspaceAccessError::InvalidSelection,
            ))?;
        self.grant_path(session, WorkspacePathPurpose::Export, selected)
    }

    fn grant_path(
        &self,
        session: Uuid,
        purpose: WorkspacePathPurpose,
        selected: PathBuf,
    ) -> Result<WorkspacePathGrant, WorkspaceThreadError> {
        self.request(|reply| Request::GrantPath(session, purpose, selected, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    pub fn consume_import_directory(
        &self,
        session: Uuid,
        grant: Uuid,
    ) -> Result<PathBuf, WorkspaceThreadError> {
        self.consume_path(session, WorkspacePathPurpose::Import, grant)
    }

    pub fn consume_video_file(
        &self,
        session: Uuid,
        grant: Uuid,
    ) -> Result<PathBuf, WorkspaceThreadError> {
        self.consume_path(session, WorkspacePathPurpose::VideoImport, grant)
    }

    #[cfg(feature = "execution-rpc")]
    pub(crate) fn consume_export_directory(
        &self,
        session: Uuid,
        grant: Uuid,
    ) -> Result<PathBuf, WorkspaceThreadError> {
        self.consume_path(session, WorkspacePathPurpose::Export, grant)
    }

    fn consume_path(
        &self,
        session: Uuid,
        purpose: WorkspacePathPurpose,
        grant: Uuid,
    ) -> Result<PathBuf, WorkspaceThreadError> {
        self.request(|reply| Request::ConsumePath(session, purpose, grant, reply))?
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

    pub fn list_datasets(
        &self,
        session: Uuid,
        after: Option<Uuid>,
        size: u16,
    ) -> Result<DatasetPage<DatasetSummary>, WorkspaceThreadError> {
        self.workspace_action(session, move |workspace| {
            workspace
                .list_datasets(after, size)
                .map_err(WorkspaceThreadError::Dataset)
        })
    }

    pub fn list_assets(
        &self,
        session: Uuid,
        dataset: Uuid,
        after: Option<Uuid>,
        size: u16,
    ) -> Result<DatasetPage<AssetSummary>, WorkspaceThreadError> {
        let (mut page, bindings) = self.workspace_action(session, move |workspace| {
            let page = workspace
                .list_assets(dataset, after, size)
                .map_err(WorkspaceThreadError::Dataset)?;
            let bindings: Vec<_> = page
                .items
                .iter()
                .map(|asset| {
                    if matches!(
                        asset.annotation_state,
                        AssetAnnotationState::ConfirmedPositive
                            | AssetAnnotationState::ConfirmedNegative
                    ) {
                        workspace.registered_image(dataset, asset.asset_id).ok()
                    } else {
                        None
                    }
                })
                .collect();
            Ok((page, bindings))
        })?;
        for (asset, binding) in page.items.iter_mut().zip(bindings) {
            asset.validate_confirmation(binding);
        }
        Ok(page)
    }

    pub fn registered_image(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
    ) -> Result<RegisteredImage, WorkspaceThreadError> {
        self.workspace_action(session, move |workspace| {
            workspace
                .registered_image(dataset, asset)
                .map_err(WorkspaceThreadError::Dataset)
        })
    }

    pub fn close(&self, expected_session_id: Uuid) -> Result<(), WorkspaceThreadError> {
        #[cfg(feature = "execution-rpc")]
        let _guard = self.lock_final_operations()?;
        self.request(|reply| Request::Close(expected_session_id, reply))?
            .map_err(WorkspaceThreadError::Access)
    }

    #[cfg(feature = "execution-rpc")]
    pub(crate) fn lock_final_operations(&self) -> Result<MutexGuard<'_, ()>, WorkspaceThreadError> {
        self.final_operations
            .lock()
            .map_err(|_| WorkspaceThreadError::Stopped)
    }

    pub fn open_review(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
        now_ms: i64,
    ) -> Result<ReviewSession, WorkspaceThreadError> {
        self.open_review_at_baseline(session, dataset, asset, None, now_ms)
    }

    pub fn open_review_checked(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
        baseline: ReviewBaseline,
        now_ms: i64,
    ) -> Result<ReviewSession, WorkspaceThreadError> {
        self.open_review_at_baseline(session, dataset, asset, Some(baseline), now_ms)
    }

    fn open_review_at_baseline(
        &self,
        session: Uuid,
        dataset: Uuid,
        asset: Uuid,
        expected: Option<ReviewBaseline>,
        now_ms: i64,
    ) -> Result<ReviewSession, WorkspaceThreadError> {
        #[cfg(feature = "execution-rpc")]
        let _guard = self.lock_final_operations()?;
        self.workspace_action(session, move |workspace| {
            workspace
                .open_review(dataset, asset, expected, now_ms)
                .map_err(WorkspaceThreadError::Review)
        })
    }

    pub fn get_review(
        &self,
        session: Uuid,
        review: Uuid,
    ) -> Result<ReviewSession, WorkspaceThreadError> {
        self.workspace_action(session, move |workspace| {
            workspace
                .get_review(review)
                .map_err(WorkspaceThreadError::Review)
        })
    }

    pub fn cancel_review(
        &self,
        session: Uuid,
        review: Uuid,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ReviewSession, WorkspaceThreadError> {
        self.workspace_action(session, move |workspace| {
            workspace
                .cancel_review(review, expected_revision, now_ms)
                .map_err(WorkspaceThreadError::Review)
        })
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

    pub fn task_outcome(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<TaskOutcome, WorkspaceThreadError> {
        self.task(session, move |workspace| workspace.task_outcome(task))
    }

    pub(crate) fn storage_root(&self, session: Uuid) -> Result<PathBuf, WorkspaceThreadError> {
        self.workspace_action(session, |workspace| Ok(workspace.root().to_owned()))
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

    #[cfg(feature = "execution-rpc")]
    pub(crate) fn report_directory(
        &self,
        scope: ReportScope,
    ) -> Result<PathBuf, WorkspaceThreadError> {
        self.task(scope.workspace_session, move |workspace| {
            workspace.report_directory(&scope)
        })
    }

    #[cfg(feature = "execution-rpc")]
    pub(crate) fn record_reported_running(
        &self,
        session: Uuid,
        grant: Uuid,
        receipt: ClaimReceipt,
        notice: (Uuid, u64),
        now_ms: i64,
    ) -> Result<(), WorkspaceThreadError> {
        self.task(session, move |workspace| {
            workspace.record_reported_running(session, grant, receipt, notice, now_ms)
        })
    }

    pub fn record_running(
        &self,
        session: Uuid,
        grant: Uuid,
        receipt: ClaimReceipt,
        now_ms: i64,
    ) -> Result<(), WorkspaceThreadError> {
        self.task(session, move |workspace| {
            workspace.record_running(session, grant, receipt, now_ms)
        })
    }

    #[cfg(feature = "execution-rpc")]
    pub fn record_success(
        &self,
        result: ResultSubmission,
        now_ms: i64,
    ) -> Result<TerminalReceipt, WorkspaceThreadError> {
        self.task(result.workspace_session, move |workspace| {
            workspace.record_success(&result, now_ms)
        })
    }

    #[cfg(feature = "execution-rpc")]
    pub(crate) fn record_failure(
        &self,
        scope: ReportScope,
        event: (Uuid, u64),
        code: &'static str,
        now_ms: i64,
    ) -> Result<TerminalReceipt, WorkspaceThreadError> {
        self.task(scope.workspace_session, move |workspace| {
            workspace.record_failure(scope, event, code, now_ms)
        })
    }

    fn task<T: Send + 'static>(
        &self,
        session: Uuid,
        execute: impl FnOnce(&mut OpenedWorkspace) -> Result<T, TaskStoreError> + Send + 'static,
    ) -> Result<T, WorkspaceThreadError> {
        self.workspace_action(session, move |workspace| {
            execute(workspace).map_err(WorkspaceThreadError::Task)
        })
    }

    pub(crate) fn commit_import(
        &self,
        session: Uuid,
        prepared: PreparedImport,
        now_ms: i64,
    ) -> Result<ImportRegistration, WorkspaceThreadError> {
        self.workspace_action(session, move |workspace| {
            workspace
                .commit_import(prepared, now_ms)
                .map_err(WorkspaceThreadError::Import)
        })
    }

    pub(crate) fn workspace_action<T: Send + 'static>(
        &self,
        session: Uuid,
        execute: impl FnOnce(&mut OpenedWorkspace) -> Result<T, WorkspaceThreadError> + Send + 'static,
    ) -> Result<T, WorkspaceThreadError> {
        self.request(|reply| {
            Request::Action(Box::new(move |access| {
                let result = access
                    .active_handle(session)
                    .map_err(WorkspaceThreadError::Access)
                    .and_then(execute);
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
