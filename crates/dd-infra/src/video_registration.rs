//! Register external video bytes and enqueue their probe as one Workspace operation.

use std::error::Error;
use std::fmt::{self, Display};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::input_fingerprint::{InputFingerprintError, freeze_input_manifest};
use crate::json::JsonContracts;
use crate::paths::{open_registered_file, open_scoped_file, publish_immutable};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, TaskSubmission, enqueue_task_in, format_digest};
use crate::workspace::OpenedWorkspace;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

const CAPABILITY: &str = "video_probe.v1";
const ACTION: &str = "VIDEO_PROBE";
const MANIFEST_LIMIT: u64 = 128 * 1024;

#[derive(Clone)]
pub struct VideoImport {
    pub request_id: Uuid,
    pub idempotency_key: Uuid,
    pub file_grant_id: Uuid,
    pub display_name: Option<String>,
    pub stream_index: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoImportReceipt {
    pub video_id: Uuid,
    pub task_id: Uuid,
}

#[derive(Debug)]
pub enum VideoImportError {
    InvalidRequest,
    InvalidManifest,
    SourceChanged,
    IdempotencyConflict,
    Setup(Box<dyn Error + Send + Sync>),
    Storage(WorkspaceThreadError),
    Task(TaskStoreError),
    Database(rusqlite::Error),
    Manifest(InputFingerprintError),
    File(io::Error),
}

impl Display for VideoImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => write!(formatter, "Invalid video import request"),
            Self::InvalidManifest => write!(formatter, "Stored video probe input is invalid"),
            Self::SourceChanged => write!(
                formatter,
                "Registered source video differs from selected bytes"
            ),
            Self::IdempotencyConflict => write!(formatter, "Video import request has changed"),
            Self::Setup(error) => write!(formatter, "Load video input contracts: {error}"),
            Self::Storage(error) => error.fmt(formatter),
            Self::Task(error) => error.fmt(formatter),
            Self::Database(error) => error.fmt(formatter),
            Self::Manifest(error) => error.fmt(formatter),
            Self::File(error) => error.fmt(formatter),
        }
    }
}

impl Error for VideoImportError {}

impl From<rusqlite::Error> for VideoImportError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<io::Error> for VideoImportError {
    fn from(error: io::Error) -> Self {
        Self::File(error)
    }
}

pub struct VideoRegistry {
    owner: Arc<WorkspaceThread>,
    contracts: Arc<JsonContracts>,
}

impl VideoRegistry {
    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, VideoImportError> {
        Ok(Self {
            owner,
            contracts: Arc::new(JsonContracts::new().map_err(VideoImportError::Setup)?),
        })
    }

    pub fn admit(
        &self,
        session: Uuid,
        mut request: VideoImport,
        now_ms: i64,
    ) -> Result<VideoImportReceipt, VideoImportError> {
        if request.request_id.is_nil()
            || request.idempotency_key.is_nil()
            || request.file_grant_id.is_nil()
        {
            return Err(VideoImportError::InvalidRequest);
        }
        if let Some(name) = &mut request.display_name {
            *name = name.trim().to_owned();
            if name.is_empty() || name.chars().count() > 128 {
                return Err(VideoImportError::InvalidRequest);
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"datasetdoctor.task.video-probe.v1\0");
        hash.update(request.file_grant_id.as_bytes());
        hash.update(
            request
                .stream_index
                .map(u64::from)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hash.update(request.display_name.as_deref().unwrap_or("").as_bytes());
        let fingerprint: [u8; 32] = hash.finalize().into();
        let key = request.idempotency_key;
        let previous = self
            .owner
            .workspace_action(session, move |workspace| {
                Ok(find_existing(
                    &workspace.connection,
                    workspace.root(),
                    key,
                    fingerprint,
                ))
            })
            .map_err(VideoImportError::Storage)??;
        if let Some(previous) = previous {
            return Ok(previous);
        }
        let selected = self
            .owner
            .consume_video_file(session, request.file_grant_id)
            .map_err(VideoImportError::Storage)?;
        let mut source = open_registered_file(&selected)?;
        let mut content = Sha256::new();
        io::copy(&mut source, &mut content)?;
        let prepared = PreparedVideo {
            selected,
            source_digest: format!("{:x}", content.finalize()),
            request,
            fingerprint,
        };
        let contracts = Arc::clone(&self.contracts);
        self.owner
            .workspace_action(session, move |workspace| {
                Ok(commit_video(
                    workspace, &contracts, session, prepared, now_ms,
                ))
            })
            .map_err(VideoImportError::Storage)?
    }
}

struct PreparedVideo {
    selected: PathBuf,
    source_digest: String,
    request: VideoImport,
    fingerprint: [u8; 32],
}

fn commit_video(
    workspace: &mut OpenedWorkspace,
    contracts: &JsonContracts,
    session: Uuid,
    prepared: PreparedVideo,
    now_ms: i64,
) -> Result<VideoImportReceipt, VideoImportError> {
    let PreparedVideo {
        selected,
        source_digest,
        request,
        fingerprint,
    } = prepared;
    let root = workspace.root().to_owned();
    let path = selected.to_str().ok_or(VideoImportError::InvalidRequest)?;
    let filename = selected
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(VideoImportError::InvalidRequest)?;
    let tx = workspace
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(existing) = find_existing(&tx, &root, request.idempotency_key, fingerprint)? {
        return Ok(existing);
    }
    let asset: Option<(Uuid, String, Option<String>)> = tx.query_row(
        "SELECT asset_id, kind, media_digest FROM main.assets WHERE location_kind = 'EXTERNAL' AND path = ?1",
        [path],
        |row| Ok((row_uuid(row, 0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let video_id = match asset {
        Some((id, kind, Some(sha))) if kind == "VIDEO" && sha == source_digest => id,
        Some(_) => return Err(VideoImportError::SourceChanged),
        None => {
            let id = Uuid::new_v4();
            tx.execute(
                "INSERT INTO main.assets
                    (asset_id, kind, location_kind, path, original_filename, file_origin, media_digest,
                     metadata_json, created_at_ms, updated_at_ms)
                 VALUES (?1, 'VIDEO', 'EXTERNAL', ?2, ?3, 'IMPORTED', ?4,
                     json_object('display_name', ?5), ?6, ?6)",
                params![id.to_string(), path, filename, source_digest,
                    request.display_name.as_deref().unwrap_or(filename), now_ms],
            )?;
            id
        }
    };
    let frozen = freeze_input_manifest(contracts, json!({
        "dataset_id": null, "label_schema_digest": null,
        "assets": [{"asset_id": video_id, "media_digest": source_digest, "annotation_digest": null,
            "source_metadata_digest": null}],
        "capability_id": CAPABILITY, "implementation_version": env!("CARGO_PKG_VERSION"),
        "model_identity": null, "resolved_config": {"stream_index": request.stream_index},
    })).map_err(VideoImportError::Manifest)?;
    let relative =
        Path::new("artifacts/task-inputs").join(format!("{}.json", format_digest(&frozen.sha256)));
    let submission = TaskSubmission {
        dataset_id: None,
        workspace_session: session,
        request_id: request.request_id,
        idempotency_key: request.idempotency_key,
        operation_kind: ACTION.into(),
        capability_id: CAPABILITY.into(),
        input_manifest_path: relative,
        request_fingerprint: fingerprint,
        input_fingerprint: frozen.input_fingerprint,
    };
    let queued = enqueue_task_in(&tx, &submission, now_ms).map_err(VideoImportError::Task)?;
    publish_immutable(&root, &submission.input_manifest_path, &frozen.bytes)?;
    tx.commit()?;
    Ok(VideoImportReceipt {
        video_id,
        task_id: queued.task_id,
    })
}

fn find_existing(
    connection: &Connection,
    root: &Path,
    key: Uuid,
    fingerprint: [u8; 32],
) -> Result<Option<VideoImportReceipt>, VideoImportError> {
    let record: Option<(Uuid, String, String)> = connection
        .query_row(
            "SELECT task_id, request_fingerprint, input_manifest_path FROM main.tasks
         WHERE operation_kind = ?1 AND idempotency_key = ?2",
            params![ACTION, key.to_string()],
            |row| Ok((row_uuid(row, 0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((task_id, stored, relative)) = record else {
        return Ok(None);
    };
    if stored != format_digest(&fingerprint) {
        return Err(VideoImportError::IdempotencyConflict);
    }
    let relative = Path::new(&relative);
    let mut bytes = Vec::new();
    open_scoped_file(root, relative)?
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MANIFEST_LIMIT
        || relative.file_stem().and_then(|name| name.to_str())
            != Some(&format!("{:x}", Sha256::digest(&bytes)))
    {
        return Err(VideoImportError::InvalidManifest);
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| VideoImportError::InvalidManifest)?;
    let video_id = manifest["assets"][0]["asset_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(VideoImportError::InvalidManifest)?;
    Ok(Some(VideoImportReceipt { video_id, task_id }))
}
