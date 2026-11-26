import { invoke } from "@tauri-apps/api/core";

import type {
  CloseReceipt,
  CommandContext,
  PathGrant,
  PathSelectRequest,
  WorkspaceCloseRequest,
  WorkspaceGetRequest,
  WorkspaceOpenRequest,
  WorkspaceSnapshot,
} from "./generated/wire-primitives";

export type OpenWorkspace = Extract<WorkspaceSnapshot, { status: "OPEN" }>;

export async function selectAndOpenWorkspace(): Promise<OpenWorkspace | null> {
  const pick: PathSelectRequest = {
    request_id: crypto.randomUUID(), purpose: "WORKSPACE", kind: "DIRECTORY", access: "READ_WRITE",
  };
  const grant = await invoke<PathGrant | null>("path_select", { request: pick });
  if (!grant) return null;
  const request: WorkspaceOpenRequest = {
    request_id: crypto.randomUUID(), directory_grant_id: grant.grant_id,
  };
  const workspace = await invoke<WorkspaceSnapshot>("workspace_open", { request });
  if (workspace.status !== "OPEN") throw new Error("Workspace open returned no active session");
  return workspace;
}

export function getWorkspace(workspace?: OpenWorkspace): Promise<WorkspaceSnapshot> {
  const request_id = crypto.randomUUID();
  const request: WorkspaceGetRequest = {
    request_id,
    ...(workspace && { context: context(workspace, request_id) }),
  };
  return invoke("workspace_get", { request });
}

export function closeWorkspace(workspace: OpenWorkspace): Promise<CloseReceipt> {
  const request: WorkspaceCloseRequest = {
    context: context(workspace, crypto.randomUUID()),
    mode: "WAIT",
    expected_session_id: workspace.workspace_session_id,
  };
  return invoke("workspace_close", { request });
}

function context(workspace: OpenWorkspace, request_id: string): CommandContext {
  return {
    request_id,
    workspace_id: workspace.workspace_id,
    workspace_session_id: workspace.workspace_session_id,
  };
}

export function workspaceCommandError(error: unknown): string {
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
    return error.message;
  }
  return "无法完成工作区操作。";
}
