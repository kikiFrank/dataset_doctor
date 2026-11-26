import { invoke } from "@tauri-apps/api/core";
import type { ImportOperationRef, ImportTaskView, PathGrant, PathSelectRequest, WorkspaceBackupSubmitRequest } from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function selectBackupDirectory(workspace: OpenWorkspace): Promise<PathGrant | null> {
  const request_id = crypto.randomUUID();
  const request: PathSelectRequest = {
    request_id, context: context(workspace, request_id), purpose: "BACKUP", kind: "DIRECTORY", access: "READ_WRITE",
  };
  return invoke("path_select", { request });
}

export function startBackup(
  workspace: OpenWorkspace, destinationGrantId: string, idempotencyKey: string,
): Promise<ImportOperationRef> {
  const request: WorkspaceBackupSubmitRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    destination_grant_id: destinationGrantId,
    include_reports: true,
  };
  return invoke("workspace_backup_submit", { request });
}

export function latestBackup(workspace: OpenWorkspace): Promise<ImportTaskView | null> {
  return invoke("workspace_backup_latest", { context: context(workspace, crypto.randomUUID()) });
}
