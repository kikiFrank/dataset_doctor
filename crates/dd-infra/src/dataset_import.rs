//! Connect the read-only COCO preview to the Core approval and SQLite commit.

use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dd_core::import_plan::{Approval, ImportApprovalBook, ImportApprovalError};
use dd_core::workspace::WorkspaceAccessError;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::import_preflight::{Annotation, ImportPreview, inspect_single_image_coco};
use crate::import_registration::{
    ImportRegistration, ImportRegistrationError, prepare_registration,
};
use crate::json::JsonContracts;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

#[derive(Debug)]
pub enum DatasetImportError {
    Approval(ImportApprovalError),
    Workspace(WorkspaceThreadError),
    Source(io::Error),
    Registration(ImportRegistrationError),
    Unavailable,
}

impl Display for DatasetImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approval(error) => Display::fmt(error, formatter),
            Self::Workspace(error) => Display::fmt(error, formatter),
            Self::Source(error) => write!(formatter, "Read the selected import sources: {error}"),
            Self::Registration(error) => Display::fmt(error, formatter),
            Self::Unavailable => formatter.write_str("Import plan state is unavailable"),
        }
    }
}

impl Error for DatasetImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Approval(error) => Some(error),
            Self::Workspace(error) => Some(error),
            Self::Source(error) => Some(error),
            Self::Registration(error) => Some(error),
            Self::Unavailable => None,
        }
    }
}

pub struct ImportPlan {
    pub plan_id: Uuid,
    pub fingerprint: [u8; 32],
    pub preview: Arc<ImportPreview>,
}

#[derive(Clone)]
struct PreviewedImport {
    dataset_name: String,
    preview: Arc<ImportPreview>,
}

pub struct DatasetImportCoordinator {
    workspace: Arc<WorkspaceThread>,
    contracts: JsonContracts,
    approvals: Mutex<ImportApprovalBook<PreviewedImport, ImportRegistration>>,
}

impl DatasetImportCoordinator {
    pub fn new(workspace: Arc<WorkspaceThread>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            workspace,
            contracts: JsonContracts::new()?,
            approvals: Mutex::new(ImportApprovalBook::default()),
        })
    }

    /// Read and hash files on the caller's I/O thread. No Dataset is registered
    /// until the caller confirms this exact preview in its active session.
    pub fn inspect(
        &self,
        session: Uuid,
        directory_grant: Uuid,
        dataset_name: String,
        selected: Vec<PathBuf>,
    ) -> Result<ImportPlan, DatasetImportError> {
        let dataset_name = dataset_name.trim().to_owned();
        if dataset_name.is_empty() {
            return Err(DatasetImportError::Registration(
                ImportRegistrationError::InvalidDatasetName,
            ));
        }
        let root = self
            .workspace
            .consume_import_directory(session, directory_grant)
            .map_err(DatasetImportError::Workspace)?;
        let preview = Arc::new(
            inspect_single_image_coco(&root, &self.contracts, &selected)
                .map_err(DatasetImportError::Source)?,
        );
        let fingerprint = plan_fingerprint(session, &dataset_name, &preview);
        let mut approvals = self
            .approvals
            .lock()
            .map_err(|_| DatasetImportError::Unavailable)?;
        self.require_session(session)?;
        let plan_id = approvals.remember(
            session,
            fingerprint,
            PreviewedImport {
                dataset_name,
                preview: Arc::clone(&preview),
            },
        );
        Ok(ImportPlan {
            plan_id,
            fingerprint,
            preview,
        })
    }

    pub fn confirm(
        &self,
        session: Uuid,
        plan_id: Uuid,
        fingerprint: [u8; 32],
        now_ms: i64,
    ) -> Result<ImportRegistration, DatasetImportError> {
        self.require_session(session)?;
        let approved = self
            .approvals
            .lock()
            .map_err(|_| DatasetImportError::Unavailable)?
            .confirm(session, plan_id, fingerprint)
            .map_err(DatasetImportError::Approval)?;
        let previewed = match approved {
            Approval::Apply(previewed) => previewed,
            Approval::Replay(receipt) => return Ok(receipt),
        };
        let result =
            prepare_registration(&self.contracts, &previewed.dataset_name, &previewed.preview)
                .map_err(DatasetImportError::Registration)
                .and_then(|prepared| {
                    self.workspace
                        .commit_import(session, prepared, now_ms)
                        .map_err(|error| match error {
                            WorkspaceThreadError::Import(error) => {
                                DatasetImportError::Registration(error)
                            }
                            other => DatasetImportError::Workspace(other),
                        })
                });
        let mut approvals = self
            .approvals
            .lock()
            .map_err(|_| DatasetImportError::Unavailable)?;
        match &result {
            Ok(receipt) => approvals.complete(plan_id, receipt.clone()),
            Err(DatasetImportError::Registration(ImportRegistrationError::InputChanged)) => {
                approvals.discard(plan_id)
            }
            Err(_) => approvals.retry(plan_id, previewed),
        }
        result
    }

    fn require_session(&self, session: Uuid) -> Result<(), DatasetImportError> {
        let error = match self
            .workspace
            .current()
            .map_err(DatasetImportError::Workspace)?
        {
            Some(current) if current.workspace_session_id == session => return Ok(()),
            Some(_) => WorkspaceAccessError::StaleSession,
            None => WorkspaceAccessError::Closed,
        };
        Err(DatasetImportError::Workspace(WorkspaceThreadError::Access(
            error,
        )))
    }
}

fn plan_fingerprint(session: Uuid, dataset_name: &str, preview: &ImportPreview) -> [u8; 32] {
    let files: Vec<_> = preview.inputs.iter().map(|input| {
        let annotation = match &input.annotation {
            Annotation::Missing => json!({"state": "MISSING"}),
            Annotation::Reference {path, sha256, ..} => json!({"state": "REFERENCE", "path": path.to_string_lossy(), "sha256": sha256}),
            Annotation::Blocked(problem) => json!({"state": "BLOCKED", "issue": format!("{problem:?}")}),
        };
        json!({"image": input.image.to_string_lossy(), "sha256": input.media_sha256, "annotation": annotation})
    }).collect();
    let body = json!({"workspace_session": session.to_string(), "dataset_name": dataset_name, "root": preview.root.to_string_lossy(), "inputs": files});
    let mut digest = Sha256::new();
    digest.update(b"datasetdoctor.dataset.import.external-coco.v1\0");
    digest.update(body.to_string());
    digest.finalize().into()
}
