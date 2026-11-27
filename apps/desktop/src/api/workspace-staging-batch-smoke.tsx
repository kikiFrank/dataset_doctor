import { useState } from "react";
import { createRoot } from "react-dom/client";
import { StagingPanel } from "../staging-panel";
import { listDatasets } from "./dataset";
import { acknowledgeWorkspaceVerification } from "./runtime";
import { listStaging } from "./staging";
import { closeWorkspace, selectAndOpenWorkspace, workspaceCommandError, type OpenWorkspace } from "./workspace";
import { waitFor } from "./workspace-workflow-smoke";

function BatchPanel({ workspace, datasetId }: { workspace: OpenWorkspace; datasetId: string }) {
  const [revision, setRevision] = useState(0);
  return <StagingPanel
    workspace={workspace}
    datasetId={datasetId}
    refreshKey={revision}
    selectionBlocked={false}
    onSelect={() => {}}
    onAccepted={() => setRevision(value => value + 1)}
  />;
}

export async function verifyWorkspaceStagingBatchIpc() {
  try {
    const workspace = await selectAndOpenWorkspace();
    if (!workspace) throw new Error("The Staging fixture did not open");
    const datasets = (await listDatasets(workspace)).items;
    if (datasets.length !== 1) throw new Error("The Staging fixture must contain one target Dataset");
    const datasetId = datasets[0].dataset_id;
    const host = document.body.appendChild(document.createElement("div"));
    const root = createRoot(host);
    root.render(<BatchPanel workspace={workspace} datasetId={datasetId} />);
    try {
      const selection = await waitFor(() => {
        const controls = Array.from(host.querySelectorAll<HTMLInputElement>('input[data-staging-batch-asset]:enabled'));
        return controls.length === 2 ? controls : null;
      }, host, "two selectable Staging rows");
      for (const row of selection) row.click();
      const prepare = await waitFor(
        () => host.querySelector<HTMLButtonElement>("button[data-staging-batch-build]:enabled"),
        host, "the batch report action",
      );
      prepare.click();
      const reason = await waitFor(
        () => host.querySelector<HTMLTextAreaElement>('textarea[data-staging-batch-reason="semantic_review"]'),
        host, "the unexecuted-check reason",
      );
      if (host.querySelector("button[data-staging-batch-commit]:enabled")
          || (await listStaging(workspace, datasetId)).items.length !== 2) {
        throw new Error("Preparing the batch bypassed its required acknowledgment");
      }
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set?.call(reason, "已核对两个合成视频帧");
      reason.dispatchEvent(new Event("input", { bubbles: true }));
      const submit = await waitFor(
        () => host.querySelector<HTMLButtonElement>("button[data-staging-batch-commit]:enabled"),
        host, "the explicit batch confirmation",
      );
      submit.click();
      await waitFor(() => {
        return host.textContent?.includes("当前没有符合条件的待接收样本。") ? host : null;
      }, host, "the refreshed empty Staging list");
    } finally {
      root.unmount();
      host.remove();
    }

    if ((await closeWorkspace(workspace)).close_state !== "CLOSED") throw new Error("Workspace did not close");
    const reopened = await selectAndOpenWorkspace();
    if (!reopened || reopened.workspace_id !== workspace.workspace_id || reopened.workspace_session_id === workspace.workspace_session_id) {
      throw new Error("Staging was not reopened in a fresh Workspace session");
    }
    if ((await listStaging(reopened, datasetId)).items.length !== 0) {
      throw new Error("The batch receipt was not reflected in Staging after reopening");
    }
    await closeWorkspace(reopened);
    await acknowledgeWorkspaceVerification();
  } catch (failure) {
    await acknowledgeWorkspaceVerification("Staging batch: " + workspaceCommandError(failure));
  }
}
