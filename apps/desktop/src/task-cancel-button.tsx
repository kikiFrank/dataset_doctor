import { useState } from "react";
import { cancelTask } from "./api/task";
import type { CancellationReceipt } from "./api/generated/wire-primitives";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { cancellationState, cancellationReceiptMessage } from "./task-cancellation-state";

export function TaskCancelButton({ workspace, taskId, status, onReceipt }: {
  workspace: OpenWorkspace; taskId: string; status: string; onReceipt?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [receipt, setReceipt] = useState<CancellationReceipt | null>(null);
  const [error, setError] = useState("");
  const state = cancellationState(status, receipt?.observed_status);
  const message = receipt && (state !== "SETTLED" || cancellationState(status) !== "SETTLED")
    ? state === "STOPPED" ? "任务已停止。" : cancellationReceiptMessage(receipt) : "";
  if ((state === "SETTLED" || state === "STOPPED") && !message) return null;
  async function cancel() {
    setBusy(true);
    setError("");
    try {
      setReceipt(await cancelTask(workspace, taskId));
      onReceipt?.();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally { setBusy(false); }
  }
  return <span>
    {(state === "AVAILABLE" || state === "REQUESTED") &&
      <button type="button" disabled={busy || state === "REQUESTED"} onClick={() => void cancel()}>
        {state === "REQUESTED" ? "等待取消确认…" : "取消任务"}
      </button>}
    {message && <span role="status">{message}</span>}
    {error && <span role="alert">{error}</span>}
  </span>;
}
