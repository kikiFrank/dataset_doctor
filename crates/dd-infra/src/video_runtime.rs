//! Dispatch registered Workspace videos and reopen their accepted display indices.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dd_domain::video::FrameIndex;
use dd_protocol::v1::{VideoProbeInput, WorkerInputFile};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_manifest::{MANIFEST_FILE, TaskManifestScope};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, JsonProfile};
use crate::paths::{is_portable_relative_file, open_registered_file, open_scoped_file};
use crate::task_store::{TaskOutcome, TaskStoreError, format_digest, parse_digest};
use crate::video_index::{VideoProbeObservation, read_video_observation};
use crate::video_registration::{CAPABILITY, VideoImportReceipt};
use crate::worker_runtime::{WorkerJob, WorkerOperation, WorkerRuntime, now_ms};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

pub use crate::worker_runtime::WorkerRuntimeError as VideoRuntimeError;

const PROBE_TIMEOUT_MS: u32 = 30_000;
const PROBE_DEADLINE: Duration = Duration::from_secs(70);
const MANIFEST_LIMIT: u64 = 128 * 1024;

pub struct VideoProbeRuntime {
    runtime: WorkerRuntime,
}

impl VideoProbeRuntime {
    pub fn task_outcome(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
    ) -> Result<TaskOutcome, WorkspaceThreadError> {
        WorkerRuntime::task_outcome(workspace, session, task, CAPABILITY)
    }

    pub fn record_unavailable(
        workspace: &WorkspaceThread,
        session: Uuid,
        task: Uuid,
        startup_failed: bool,
    ) -> Result<(), VideoRuntimeError> {
        WorkerRuntime::record_unavailable(workspace, session, task, CAPABILITY, startup_failed)
    }

    pub async fn launch(
        workspace: Arc<WorkspaceThread>,
        session: Uuid,
        interpreter: PathBuf,
        entrypoint: PathBuf,
    ) -> Result<Self, VideoRuntimeError> {
        Ok(Self {
            runtime: WorkerRuntime::launch(workspace, session, interpreter, entrypoint, CAPABILITY)
                .await?,
        })
    }

    pub async fn run(
        &mut self,
        receipt: VideoImportReceipt,
    ) -> Result<TaskOutcome, VideoRuntimeError> {
        let workspace = Arc::clone(&self.runtime.workspace);
        let session = self.runtime.session.workspace_session_id;
        let binding = tokio::task::spawn_blocking(move || {
            let binding = load_binding(&workspace, session, receipt)?;
            if binding.record.availability != "ONLINE" {
                return Err(VideoRuntimeError::InvalidBinding);
            }
            if binding.record.status == "QUEUED" {
                let expected = format_digest(&binding.input_fingerprint);
                workspace.workspace_action(session, move |owner| {
                    let switched = owner
                        .connection
                        .execute(
                            "UPDATE main.tasks SET workspace_session_id = ?4, updated_at_ms = ?5,
                               row_revision = row_revision + 1
                         WHERE task_id = ?1 AND capability_id = ?2 AND input_fingerprint = ?3
                           AND status = 'QUEUED' AND workspace_session_id <> ?4",
                            params![
                                receipt.task_id.to_string(),
                                CAPABILITY,
                                expected,
                                session.to_string(),
                                now_ms().map_err(|_| WorkspaceThreadError::Stopped)?
                            ],
                        )
                        .map_err(TaskStoreError::Database)
                        .map_err(WorkspaceThreadError::Task)?;
                    debug_assert!(switched <= 1);
                    Ok(())
                })?;
            }
            Ok::<_, VideoRuntimeError>(binding)
        })
        .await
        .map_err(|_| VideoRuntimeError::CoreStopped)??;
        if binding.record.status != "QUEUED" {
            return self.runtime.outcome(receipt.task_id).await;
        }
        let parameters = VideoProbeInput {
            source_video: Some(WorkerInputFile {
                path: path_text(&binding.source_path)?,
                sha256: format_digest(&binding.source_digest),
            }),
            input_manifest: Some(WorkerInputFile {
                path: path_text(&binding.record.root.join(&binding.record.input_path))?,
                sha256: format_digest(&binding.manifest_sha),
            }),
            stream_index: binding.stream_index,
            max_pixels: 100_000_000,
            max_frames: 100_000,
            timeout_ms: PROBE_TIMEOUT_MS,
            ..Default::default()
        };
        self.runtime
            .execute(
                WorkerJob {
                    task: receipt.task_id,
                    input_fingerprint: binding.input_fingerprint,
                    manifest_sha: binding.manifest_sha,
                    manifest_size: binding.manifest_size,
                    operation: WorkerOperation::VideoProbe(parameters),
                },
                PROBE_DEADLINE,
            )
            .await
    }

    pub async fn shutdown(self) -> Result<(), VideoRuntimeError> {
        self.runtime.shutdown().await
    }

    pub async fn terminate(self) -> Result<(), VideoRuntimeError> {
        self.runtime.terminate().await
    }
}

pub fn read_registered_video_index(
    workspace: &WorkspaceThread,
    session: Uuid,
    receipt: VideoImportReceipt,
) -> Result<FrameIndex, VideoRuntimeError> {
    read_registered_video_observation(workspace, session, receipt)
        .map(|observation| observation.index)
}

pub fn read_registered_video_observation(
    workspace: &WorkspaceThread,
    session: Uuid,
    receipt: VideoImportReceipt,
) -> Result<VideoProbeObservation, VideoRuntimeError> {
    let binding = load_binding(workspace, session, receipt)?;
    if binding.record.status != "SUCCEEDED" {
        return Err(VideoRuntimeError::ResultUnconfirmed);
    }
    verify_live_source(&binding)?;
    read_bound_video_observation(binding, receipt)
}

pub(crate) fn read_stored_video_observation(
    workspace: &WorkspaceThread,
    session: Uuid,
    receipt: VideoImportReceipt,
) -> Result<VideoProbeObservation, VideoRuntimeError> {
    let binding = load_binding(workspace, session, receipt)?;
    if binding.record.status != "SUCCEEDED" {
        return Err(VideoRuntimeError::ResultUnconfirmed);
    }
    read_bound_video_observation(binding, receipt)
}

fn read_bound_video_observation(
    binding: ProbeBinding,
    receipt: VideoImportReceipt,
) -> Result<VideoProbeObservation, VideoRuntimeError> {
    let attempt = binding
        .record
        .attempt
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(VideoRuntimeError::InvalidBinding)?;
    let relative = PathBuf::from(format!("artifacts/tasks/{}/{attempt}", receipt.task_id));
    if binding.record.output_path.as_deref().map(Path::new)
        != Some(relative.join(MANIFEST_FILE).as_path())
    {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    let directory = binding.record.root.join(relative);
    if directory.canonicalize()? != directory {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    let reference = crate::artifact_manifest::parse_stored_manifest_reference(
        binding
            .record
            .manifest_ref
            .as_deref()
            .ok_or(VideoRuntimeError::InvalidBinding)?,
        receipt.task_id,
    )
    .map_err(|error| VideoRuntimeError::TaskEvidence(Box::new(error)))?;
    read_video_observation(
        &directory,
        &reference,
        &TaskManifestScope {
            task: receipt.task_id,
            attempt,
            input_fingerprint: binding.input_fingerprint,
        },
        receipt.video_id,
        binding.source_digest,
        binding.stream_index,
    )
    .map_err(VideoRuntimeError::TaskEvidence)
}

struct StoredProbe {
    root: PathBuf,
    status: String,
    input_path: String,
    input_fingerprint: String,
    source_path: String,
    source_location: String,
    availability: String,
    source_digest: String,
    attempt: Option<String>,
    output_path: Option<String>,
    manifest_ref: Option<String>,
}

struct ProbeBinding {
    record: StoredProbe,
    source_path: PathBuf,
    source_digest: [u8; 32],
    input_fingerprint: [u8; 32],
    manifest_sha: [u8; 32],
    manifest_size: u64,
    stream_index: Option<u32>,
}

fn load_binding(
    workspace: &WorkspaceThread,
    session: Uuid,
    receipt: VideoImportReceipt,
) -> Result<ProbeBinding, VideoRuntimeError> {
    let record = workspace.workspace_action(session, move |owner| {
        owner.connection.query_row(
            "SELECT t.status, t.input_manifest_path, t.input_fingerprint, a.path, a.location_kind,
                    a.media_digest, t.current_attempt_id, t.output_manifest_path,
                    json_extract(t.execution_metadata_json, '$.terminal_receipt.result_manifest_ref'), a.availability
             FROM main.tasks t JOIN main.assets a ON a.asset_id = ?2
             WHERE t.task_id = ?1 AND t.capability_id = ?3 AND a.kind = 'VIDEO'",
            params![receipt.task_id.to_string(), receipt.video_id.to_string(), CAPABILITY],
            |row| Ok(StoredProbe {
                root: owner.root().to_owned(), status: row.get(0)?, input_path: row.get(1)?,
                input_fingerprint: row.get(2)?, source_path: row.get(3)?, source_location: row.get(4)?,
                source_digest: row.get(5)?, attempt: row.get(6)?, output_path: row.get(7)?, manifest_ref: row.get(8)?, availability: row.get(9)?,
            }),
        ).optional().map_err(TaskStoreError::Database).map_err(WorkspaceThreadError::Task)?
            .ok_or(WorkspaceThreadError::Task(TaskStoreError::UnknownTask))
    })?;
    let mut input = Vec::new();
    open_scoped_file(&record.root, Path::new(&record.input_path))?
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut input)?;
    if input.len() as u64 > MANIFEST_LIMIT {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    let contracts = JsonContracts::new().map_err(VideoRuntimeError::TaskEvidence)?;
    let value = contracts
        .validate(JsonProfile::InputManifest, &input)
        .map_err(|error| VideoRuntimeError::TaskEvidence(Box::new(error)))?;
    let frozen =
        freeze_input_manifest(&contracts, value.clone()).map_err(VideoRuntimeError::Fingerprint)?;
    let [asset] = value["assets"]
        .as_array()
        .ok_or(VideoRuntimeError::InvalidBinding)?
        .as_slice()
    else {
        return Err(VideoRuntimeError::InvalidBinding);
    };
    if input != frozen.bytes
        || record.input_fingerprint != format_digest(&frozen.input_fingerprint)
        || value["capability_id"] != CAPABILITY
        || asset["asset_id"] != receipt.video_id.to_string()
        || asset["media_digest"] != record.source_digest
    {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    let stream_index = serde_json::from_value(value["resolved_config"]["stream_index"].clone())
        .map_err(|_| VideoRuntimeError::InvalidBinding)?;
    let path = Path::new(&record.source_path);
    let source_path = match record.source_location.as_str() {
        "EXTERNAL" if path.is_absolute() => path.to_owned(),
        "WORKSPACE" if is_portable_relative_file(path) => record.root.join(path),
        _ => return Err(VideoRuntimeError::InvalidBinding),
    };
    let source_digest =
        parse_digest(&record.source_digest).ok_or(VideoRuntimeError::InvalidBinding)?;
    Ok(ProbeBinding {
        record,
        source_path,
        source_digest,
        input_fingerprint: frozen.input_fingerprint,
        manifest_sha: frozen.sha256,
        manifest_size: frozen.bytes.len() as u64,
        stream_index,
    })
}

fn verify_live_source(binding: &ProbeBinding) -> Result<(), VideoRuntimeError> {
    if binding.record.availability != "ONLINE" {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    let mut source = if binding.record.source_location == "EXTERNAL" {
        open_registered_file(&binding.source_path)?
    } else {
        open_scoped_file(&binding.record.root, Path::new(&binding.record.source_path))?
    };
    let mut hash = Sha256::new();
    io::copy(&mut source, &mut hash)?;
    if <[u8; 32]>::from(hash.finalize()) != binding.source_digest {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<String, VideoRuntimeError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(VideoRuntimeError::InvalidBinding)
}
