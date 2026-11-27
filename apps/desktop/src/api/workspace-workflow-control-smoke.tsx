import { createRoot } from "react-dom/client";
import { WorkflowPanel } from "../workflow-panel";
import { listWorkflows } from "./workflow";
import { closeWorkspace, selectAndOpenWorkspace, type OpenWorkspace } from "./workspace";
import { waitFor } from "./workspace-workflow-smoke";

async function withPanel<T>(
  workspace: OpenWorkspace, datasetId: string, drive: (host: HTMLElement) => Promise<T>,
): Promise<T> {
  const host = document.body.appendChild(document.createElement("div"));
  const root = createRoot(host);
  root.render(<WorkflowPanel workspace={workspace} datasetId={datasetId} />);
  try {
    return await drive(host);
  } finally {
    root.unmount();
    host.remove();
  }
}

function enabledButton(host: HTMLElement, label: string): HTMLButtonElement | null {
  return Array.from(host.querySelectorAll<HTMLButtonElement>("button:enabled"))
    .find(button => button.textContent === label) ?? null;
}

export async function verifyWorkspaceWorkflowControl(workspace: OpenWorkspace, datasetId: string) {
  const paused = await withPanel(workspace, datasetId, async host => {
    (await waitFor(() => enabledButton(host, "开始本地体检"), host, "the healthcheck start button")).click();
    const pause = await waitFor(() => enabledButton(host, "暂停体检"), host, "the workflow pause button");
    const [before] = (await listWorkflows(workspace, datasetId)).items;
    if (before?.status !== "RUNNING" || before.profile_task?.status !== "QUEUED") {
      throw new Error("The real fixture did not contain the controlled admission boundary");
    }
    pause.click();
    await waitFor(() => host.textContent?.includes("体检已暂停") ? host : null, host, "the persisted paused state");
    const [after] = (await listWorkflows(workspace, datasetId)).items;
    if (after?.status !== "PAUSED" || after.workflow_run_id !== before.workflow_run_id
        || after.profile_task?.task_id !== before.profile_task.task_id || after.profile_task?.status !== "QUEUED") {
      throw new Error("The pause did not keep the original queued task");
    }
    return after;
  });

  if ((await closeWorkspace(workspace)).close_state !== "CLOSED") throw new Error("Cannot close the paused Workspace");
  const reopened = await selectAndOpenWorkspace();
  if (!reopened || reopened.workspace_id !== workspace.workspace_id || reopened.workspace_session_id === workspace.workspace_session_id) {
    throw new Error("The paused run did not reopen in a new Workspace session");
  }
  await withPanel(reopened, datasetId, async host => {
    const resume = await waitFor(() => enabledButton(host, "继续体检"), host, "the workflow resume button");
    if (!host.textContent?.includes("体检已暂停")) throw new Error("The resumed Workspace lost its pause state");
    resume.click();
    const report = await waitFor(() => host.querySelector('section[aria-label="质量报告"]'), host, "the resumed report");
    if (!host.textContent?.includes("等待人工审核") || !report.textContent?.includes("bbox_out_of_bounds")) {
      throw new Error("The resumed real workflow did not reach its human approval gate");
    }
    const history = (await listWorkflows(reopened, datasetId)).items;
    if (history.length !== 1 || history[0].workflow_run_id !== paused.workflow_run_id
        || history[0].profile_task?.task_id !== paused.profile_task?.task_id
        || history[0].profile_task?.status !== "SUCCEEDED") {
      throw new Error("The desktop resume started a different run or profile task");
    }
  });
  if ((await closeWorkspace(reopened)).close_state !== "CLOSED") throw new Error("Cannot close the verified Workspace");
}
