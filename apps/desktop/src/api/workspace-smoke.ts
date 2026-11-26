import { acknowledgeWorkspaceVerification } from "./runtime";
import { closeWorkspace, getWorkspace, selectAndCreateWorkspace, selectAndOpenWorkspace } from "./workspace";

export async function verifyWorkspaceIpc() {
  const first = await selectAndOpenWorkspace();
  if (
    !first || first.display_name !== "桌面验收 Workspace" ||
    first.config_revision !== "7" || first.offline_asset_count !== "1"
  ) {
    throw new Error("Workspace fixture identity or SQL fields did not reach the WebView");
  }
  const queried = await getWorkspace(first);
  if (queried.status !== "OPEN" || queried.workspace_session_id !== first.workspace_session_id) {
    throw new Error("Workspace query returned the wrong session");
  }
  const closed = await closeWorkspace(first);
  if (closed.close_state !== "CLOSED") throw new Error("Workspace close did not finish");
  const reopened = await selectAndOpenWorkspace();
  if (!reopened || reopened.workspace_session_id === first.workspace_session_id) {
    throw new Error("Workspace reopen did not create a new session");
  }
  const stale = await closeWorkspace(first).catch((error: unknown) => error);
  if (!stale || typeof stale !== "object" || !("code" in stale) || stale.code !== "STALE_WORKSPACE_SESSION") {
    throw new Error("An old Workspace session was not rejected");
  }
  await closeWorkspace(reopened);
  if ((await getWorkspace()).status !== "CLOSED") throw new Error("Workspace is still open");
  await acknowledgeWorkspaceVerification();
}

export async function verifyWorkspaceCreationIpc() {
  const name = "创建验收 Workspace";
  const created = await selectAndCreateWorkspace(name);
  if (!created || created.display_name !== name || created.config_revision !== "0" || created.offline_asset_count !== "0") {
    throw new Error("Workspace creation did not return an empty active Workspace");
  }
  await closeWorkspace(created);

  const duplicate = await selectAndCreateWorkspace("Different").catch((error: unknown) => error);
  if (!duplicate || typeof duplicate !== "object" || !("code" in duplicate) || duplicate.code !== "FAILED_PRECONDITION") {
    throw new Error("Duplicate Workspace creation was not rejected");
  }
  const reopened = await selectAndOpenWorkspace();
  if (!reopened || reopened.workspace_id !== created.workspace_id || reopened.workspace_session_id === created.workspace_session_id) {
    throw new Error("The created Workspace could not be reopened with a fresh session");
  }
  await closeWorkspace(reopened);
  await acknowledgeWorkspaceVerification();
}
