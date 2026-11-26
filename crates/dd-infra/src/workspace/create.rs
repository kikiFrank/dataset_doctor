use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use tempfile::Builder;

use super::{
    DATABASE_FILE, JsonContracts, MARKER_FILE, OpenedWorkspace, WorkspaceLock, WorkspaceOpenError,
    connect, finish_open, open_existing_workspace_database, parse_marker, read_marker,
};
use crate::sqlite::migrate_workspace_database;

const PENDING: &str = "workspace.json.pending";

/// Creates a Workspace in a directory granted by Core. Publish the marker last;
/// open_workspace will only recognize the directory after that point.
pub fn create_workspace(
    approved_root: &Path,
    contracts: &JsonContracts,
    busy_timeout_ms: u32,
    display_name: &str,
    workspace_id: &str,
    now_ms: i64,
) -> Result<OpenedWorkspace, WorkspaceOpenError> {
    let document = serde_json::json!({
        "config_format_version": 1, "config_revision": 0,
        "workspace_id": workspace_id, "name": display_name,
    });
    let proposed = serde_json::to_vec(&document).map_err(WorkspaceOpenError::MarkerMapping)?;
    let marker = parse_marker(contracts, &proposed)?;
    let root = approved_root
        .canonicalize()
        .map_err(WorkspaceOpenError::Io)?;
    let lock = WorkspaceLock::acquire(&root).map_err(WorkspaceOpenError::Lock)?;
    let official = root.join(MARKER_FILE);
    if entry_exists(&official)? {
        return Err(WorkspaceOpenError::ExistingWorkspace);
    }

    let pending = root.join(PENDING);
    let database = root.join(DATABASE_FILE);
    let database_exists = entry_exists(&database)?;
    if !database_exists {
        for sidecar in [
            "workspace.db-wal",
            "workspace.db-shm",
            "workspace.db-journal",
        ] {
            if entry_exists(&root.join(sidecar))? {
                return Err(WorkspaceOpenError::UnownedDatabase);
            }
        }
    }
    let (bytes, marker) = if entry_exists(&pending)? {
        let saved = read_marker(&pending, PENDING, contracts)?;
        if saved.1.name != display_name {
            return Err(WorkspaceOpenError::PendingNameMismatch);
        }
        saved
    } else {
        if database_exists {
            return Err(WorkspaceOpenError::UnownedDatabase);
        }
        publish_marker(&root, &pending, &proposed)?;
        (proposed, marker)
    };

    if !database_exists {
        let stage = Builder::new()
            .prefix(".workspace-db-")
            .tempfile_in(&root)
            .map_err(WorkspaceOpenError::Io)?
            .into_temp_path();
        let connection = migrate_workspace_database(
            connect(&stage)?,
            busy_timeout_ms,
            &marker.workspace_id,
            now_ms,
        )
        .map_err(WorkspaceOpenError::Migration)?;
        let busy: i64 = connection
            .query_row("PRAGMA main.wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(WorkspaceOpenError::Database)?;
        if busy != 0 {
            return Err(WorkspaceOpenError::CheckpointBusy);
        }
        connection
            .close()
            .map_err(|(_, error)| WorkspaceOpenError::Database(error))?;
        stage
            .persist_noclobber(&database)
            .map_err(|error| WorkspaceOpenError::Io(error.error))?;
    }

    let connection = open_existing_workspace_database(
        connect(&database)?,
        busy_timeout_ms,
        &marker.workspace_id,
    )
    .map_err(WorkspaceOpenError::Migration)?;
    let opened = finish_open(root.clone(), lock, marker, connection)?;
    publish_marker(&root, &official, &bytes)?;
    // The official marker is the commit point. A leftover intent is ignored.
    let _ = fs::remove_file(pending);
    Ok(opened)
}

fn entry_exists(path: &Path) -> Result<bool, WorkspaceOpenError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(WorkspaceOpenError::Io(error)),
    }
}

fn publish_marker(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), WorkspaceOpenError> {
    let mut staged = Builder::new()
        .prefix(".workspace-marker-")
        .tempfile_in(root)
        .map_err(WorkspaceOpenError::Io)?;
    staged.write_all(bytes).map_err(WorkspaceOpenError::Io)?;
    staged
        .as_file()
        .sync_all()
        .map_err(WorkspaceOpenError::Io)?;
    staged
        .persist_noclobber(path)
        .map_err(|error| WorkspaceOpenError::Io(error.error))?;
    Ok(())
}
