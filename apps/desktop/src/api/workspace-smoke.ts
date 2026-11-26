import { acknowledgeWorkspaceVerification } from "./runtime";
import { closeWorkspace, getWorkspace, selectAndOpenWorkspace } from "./workspace";

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
