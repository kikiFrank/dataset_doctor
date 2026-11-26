import { createRoot } from "react-dom/client";
import { WorkflowPanel } from "../workflow-panel";
import { closeWorkspace, selectAndOpenWorkspace, type OpenWorkspace } from "./workspace";

async function waitFor<T>(read: () => T | null, host: HTMLElement, description: string): Promise<T> {
  const deadline = performance.now() + 20_000;
  do {
    const error = host.querySelector('[role="alert"]')?.textContent;
    if (error) throw new Error(error);
    const result = read();
    if (result !== null) return result;
    await new Promise(resolve => setTimeout(resolve, 40));
  } while (performance.now() < deadline);
  throw new Error("Timed out waiting for " + description);
}

async function verifyPanel(workspace: OpenWorkspace, datasetId: string, submit: boolean) {
  const host = document.body.appendChild(document.createElement("div"));
  const root = createRoot(host);
  root.render(<WorkflowPanel workspace={workspace} datasetId={datasetId} />);
  try {
    if (submit) {
      const button = await waitFor(() => host.querySelector<HTMLButtonElement>("button[type=submit]:enabled"), host, "the healthcheck start button");
      button.click();
    }
    const report = await waitFor(() => host.querySelector('section[aria-label="质量报告"]'), host, "the verified workflow report");
    const body = report.textContent ?? "";
    if (!host.textContent?.includes("等待人工审核") || !body.includes("bbox_out_of_bounds")
        || !body.includes("1 / 1 个样本") || !body.includes("未配置模型")) {
      throw new Error("The workflow panel did not show the actual findings, coverage and review state");
    }
  } finally {
    root.unmount();
    host.remove();
  }
}

export async function verifyWorkspaceWorkflowPanel(workspace: OpenWorkspace, datasetId: string) {
  await verifyPanel(workspace, datasetId, true);
  if ((await closeWorkspace(workspace)).close_state !== "CLOSED") throw new Error("Workspace did not close");
  const reopened = await selectAndOpenWorkspace();
  if (!reopened || reopened.workspace_id !== workspace.workspace_id || reopened.workspace_session_id === workspace.workspace_session_id) {
    throw new Error("Workflow history was not tested in a new Workspace session");
  }
  await verifyPanel(reopened, datasetId, false);
  await closeWorkspace(reopened);
}
