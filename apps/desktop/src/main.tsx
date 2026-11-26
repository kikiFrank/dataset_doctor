import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import { acknowledgeDesktopRuntime } from "./api/runtime";
import { closeWorkspace, selectAndOpenWorkspace, workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";
import { verifyWorkspaceIpc } from "./api/workspace-smoke";
import "./style.css";

function App() {
  const [status, setStatus] = useState("正在连接桌面运行时…");
  const [runtimeConnected, setRuntimeConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  const [workspace, setWorkspace] = useState<OpenWorkspace | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void acknowledgeDesktopRuntime().then(async (runtime) => {
      setStatus(`桌面运行时已连接 · ${runtime.version}`);
      setRuntimeConnected(true);
      if (runtime.verify_workspace) await verifyWorkspaceIpc();
    }).catch((failure: unknown) => setError(workspaceCommandError(failure)));
  }, []);

  async function toggleWorkspace() {
    setError(null);
    setBusy(true);
    try {
      if (workspace) {
        const closed = await closeWorkspace(workspace);
        if (closed.close_state === "CLOSED") setWorkspace(null);
      } else {
        const opened = await selectAndOpenWorkspace();
        if (opened) setWorkspace(opened);
      }
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="app">
      <header><span className="brand-mark" aria-hidden="true">D</span>Dataset Doctor</header>
      <main>
        <h1>{workspace?.display_name ?? "工作区"}</h1>
        <p role="status">{status}</p>
        {workspace && <p>离线资源 {workspace.offline_asset_count}</p>}
        <button type="button" disabled={busy || !runtimeConnected} onClick={() => void toggleWorkspace()}>
          {workspace ? "关闭工作区" : "打开工作区"}
        </button>
        {error && <p role="alert">{error}</p>}
      </main>
    </div>
  );
}

const root = document.getElementById("root");
if (!root) throw new Error("Missing desktop root element");
createRoot(root).render(<App />);
