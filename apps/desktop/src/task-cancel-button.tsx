import { useEffect, useState } from "react";
import { cancelTask } from "./api/task";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { cancellationState, cancellationReceiptMessage } from "./task-cancellation-state";

export function TaskCancelButton({ workspace, taskId, status }: {
  workspace: OpenWorkspace; taskId: string; status: string;
}) {
  const [busy, setBusy] = useState(false);
  const [requested, setRequested] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  useEffect(() => { setRequested(false); setMessage(""); setError(""); }, [taskId]);
  const state = cancellationState(status);
  if (state === "SETTLED" || state === "STOPPED") return null;
  async function cancel() {
    setBusy(true);
    setError("");
    try {
      const receipt = await cancelTask(workspace, taskId);
      setRequested(receipt.requested);
      setMessage(cancellationReceiptMessage(receipt));
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally { setBusy(false); }
  }
  return <span>
    <button type="button" disabled={busy || requested || state === "REQUESTED"} onClick={() => void cancel()}>
      {requested || state === "REQUESTED" ? "等待取消确认…" : "取消任务"}
    </button>
    {message && <span role="status">{message}</span>}
    {error && <span role="alert">{error}</span>}
  </span>;
}
