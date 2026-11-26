//! Query a registered video's source and verified decoder observation.

use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dataset_query::DatasetQueryError;
use crate::paths::{is_portable_relative_file, open_registered_file, open_scoped_file};
use crate::sqlite::row_uuid;
use crate::task_store::{TaskStoreError, format_digest, parse_digest};
use crate::video_index::VideoMetadata;
use crate::video_registration::{CAPABILITY, VideoImportReceipt, read_video_id};
use crate::video_runtime::{VideoRuntimeError, read_stored_video_observation};
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoSourceAvailability {
    Online,
    Offline,
    Changed,
}

pub struct RegisteredVideoDetail {
    pub video_id: Uuid,
    pub display_name: String,
    pub revision: u64,
    pub source_digest: String,
    pub source_availability: VideoSourceAvailability,
    pub probe_status: Option<String>,
    pub metadata: Option<VideoMetadata>,
    pub rotation_ccw: Option<u16>,
}

struct RegisteredSource {
    root: PathBuf,
    path: PathBuf,
    external: bool,
    declared_online: bool,
    digest: [u8; 32],
    display_name: String,
    revision: u64,
    probe_hint: Option<String>,
    probes: Vec<(Uuid, String, PathBuf, i64)>,
}

pub fn registered_video_detail(
    owner: &WorkspaceThread,
    session: Uuid,
    video: Uuid,
) -> Result<RegisteredVideoDetail, VideoRuntimeError> {
    let source = owner.workspace_action(session, move |workspace| {
        let record = workspace.connection.query_row(
            "SELECT path, location_kind, media_digest, availability,
                    COALESCE(json_extract(metadata_json,'$.display_name'),original_filename),row_revision,
                    json_extract(metadata_json,'$.video_probe_task_id')
             FROM assets WHERE asset_id=?1 AND kind='VIDEO'",
            [video.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, i64>(5)?, row.get::<_, Option<String>>(6)?)),
        ).optional().map_err(database)?
            .ok_or(WorkspaceThreadError::Dataset(DatasetQueryError::UnknownAsset))?;
        let (path, location, digest, availability, display_name, revision, probe_hint) = record;
        let revision = u64::try_from(revision)
            .map_err(|_| WorkspaceThreadError::Task(TaskStoreError::InvalidManifestPath))?;
        if probe_hint.as_ref().is_some_and(|id| Uuid::parse_str(id).is_err()) {
            return Err(WorkspaceThreadError::Task(TaskStoreError::InvalidManifestPath));
        }
        let path = PathBuf::from(path);
        let external = location == "EXTERNAL" && path.is_absolute();
        if !external && (location != "WORKSPACE" || !is_portable_relative_file(&path)) {
            return Err(WorkspaceThreadError::Task(TaskStoreError::InvalidManifestPath));
        }
        let digest = parse_digest(&digest)
            .ok_or(WorkspaceThreadError::Task(TaskStoreError::InvalidManifestPath))?;
        let mut query = workspace.connection.prepare(
            "SELECT task_id,status,input_manifest_path,created_at_ms FROM tasks
             WHERE capability_id=?1 AND operation_kind='VIDEO_PROBE'
               AND (?2 IS NULL OR task_id=?2)
             ORDER BY created_at_ms DESC, rowid DESC",
        ).map_err(database)?;
        let probes = query.query_map(params![CAPABILITY, probe_hint], |row| {
            Ok((row_uuid(row, 0)?, row.get(1)?, PathBuf::from(row.get::<_, String>(2)?), row.get(3)?))
        }).map_err(database)?.collect::<rusqlite::Result<Vec<_>>>().map_err(database)?;
        Ok(RegisteredSource {
            root: workspace.root().to_owned(), path, external, digest, display_name, revision, probe_hint, probes,
            declared_online: availability == "ONLINE",
        })
    })?;
    let availability = source_availability(&source);
    let mut detail = RegisteredVideoDetail {
        video_id: video,
        display_name: source.display_name,
        revision: source.revision,
        source_digest: format_digest(&source.digest),
        source_availability: availability,
        probe_status: None,
        metadata: None,
        rotation_ccw: None,
    };
    for (task_id, status, path, accepted_at_ms) in source.probes {
        if read_video_id(&source.root, &path).ok() != Some(video) {
            if source.probe_hint.is_some() {
                return Err(VideoRuntimeError::InvalidBinding);
            }
            continue;
        }
        detail.probe_status = Some(status.clone());
        if status == "SUCCEEDED" {
            let stored = read_stored_video_observation(
                owner,
                session,
                VideoImportReceipt {
                    video_id: video,
                    task_id,
                    accepted_at_ms,
                },
            )?;
            detail.metadata = stored.metadata;
            detail.rotation_ccw = Some(stored.rotation_ccw);
        }
        break;
    }
    if source.probe_hint.is_some() && detail.probe_status.is_none() {
        return Err(VideoRuntimeError::InvalidBinding);
    }
    // Revalidate the session after checking files outside the database owner's thread.
    owner
        .current()?
        .filter(|current| current.workspace_session_id == session)
        .ok_or(VideoRuntimeError::StaleWorkspace)?;
    Ok(detail)
}

fn source_availability(source: &RegisteredSource) -> VideoSourceAvailability {
    if !source.declared_online {
        return VideoSourceAvailability::Offline;
    }
    let opened = if source.external {
        open_registered_file(&source.path)
    } else {
        open_scoped_file(&source.root, Path::new(&source.path))
    };
    let Ok(mut file) = opened else {
        return VideoSourceAvailability::Offline;
    };
    let mut hash = Sha256::new();
    if io::copy(&mut file, &mut hash).is_err() {
        return VideoSourceAvailability::Offline;
    }
    if <[u8; 32]>::from(hash.finalize()) == source.digest {
        VideoSourceAvailability::Online
    } else {
        VideoSourceAvailability::Changed
    }
}

fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
