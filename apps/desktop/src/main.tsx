import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import { acknowledgeDesktopRuntime } from "./api/runtime";
import { closeWorkspace, selectAndCreateWorkspace, selectAndOpenWorkspace, selectAndRestoreWorkspace, workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";
import { verifyWorkspaceCreationIpc, verifyWorkspaceIpc } from "./api/workspace-smoke";
import { DatasetBrowser } from "./dataset-browser";
import { VideoImportPanel } from "./video-import-panel";
import { WorkspaceBackupPanel } from "./workspace-backup-panel";
import "./style.css";

function App() {
  const [status, setStatus] = useState("正在连接桌面运行时…");
  const [runtimeConnected, setRuntimeConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  const [workspace, setWorkspace] = useState<OpenWorkspace | null>(null);
  const [name, setName] = useState("");
  const [restoreFolder, setRestoreFolder] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [reviewBlocked, setReviewBlocked] = useState(false);
  const [videoEditBlocked, setVideoEditBlocked] = useState(false);

  useEffect(() => {
    void acknowledgeDesktopRuntime().then(async (runtime) => {
      setStatus(`桌面运行时已连接 · ${runtime.version}`);
      setRuntimeConnected(true);
      if (runtime.verify_workspace) await verifyWorkspaceIpc();
      if (runtime.verify_workspace_create) await verifyWorkspaceCreationIpc();
    }).catch((failure: unknown) => setError(workspaceCommandError(failure)));
  }, []);

  async function runWorkspaceAction(action: () => Promise<void>) {
    setError(null);
    setBusy(true);
    try {
      await action();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function toggleWorkspace() {
    await runWorkspaceAction(async () => {
      if (workspace) {
        const closed = await closeWorkspace(workspace);
        if (closed.close_state === "CLOSED") setWorkspace(null);
      } else {
        const opened = await selectAndOpenWorkspace();
        if (opened) setWorkspace(opened);
      }
    });
  }

  async function createWorkspace() {
    await runWorkspaceAction(async () => {
      const created = await selectAndCreateWorkspace(name.trim());
      if (created) {
        setName("");
        setWorkspace(created);
      }
    });
  }

  async function restoreWorkspace() {
    await runWorkspaceAction(async () => {
      const restored = await selectAndRestoreWorkspace(restoreFolder.trim());
      if (restored) {
        setRestoreFolder("");
        setWorkspace(restored);
      }
    });
  }

  return (
    <div className="app">
      <header><span className="brand-mark" aria-hidden="true">D</span>Dataset Doctor</header>
      <main>
        <h1>{workspace?.display_name ?? "工作区"}</h1>
        <p role="status">{status}</p>
        {workspace && <p>离线资源 {workspace.offline_asset_count}</p>}
        <button type="button" disabled={busy || !runtimeConnected || reviewBlocked || videoEditBlocked} onClick={() => void toggleWorkspace()}>
          {workspace ? "关闭工作区" : "打开工作区"}
        </button>
        {!workspace && (
          <form className="workspace-create-form" onSubmit={(event) => { event.preventDefault(); void createWorkspace(); }}>
            <label htmlFor="workspace-name">工作区名称</label>
            <div className="workspace-create">
              <input id="workspace-name" value={name} required disabled={busy} onChange={(event) => setName(event.target.value)} />
              <button type="submit" disabled={busy || !runtimeConnected || !name.trim() || [...name.trim()].length > 128}>
                选择目录并创建
              </button>
            </div>
          </form>
        )}
        {!workspace && (
          <form className="workspace-create-form" onSubmit={(event) => { event.preventDefault(); void restoreWorkspace(); }}>
            <label htmlFor="restore-folder">恢复目录名称</label>
            <div className="workspace-create">
              <input id="restore-folder" value={restoreFolder} required disabled={busy}
                onChange={(event) => setRestoreFolder(event.target.value)} />
              <button type="submit" disabled={busy || !runtimeConnected || !restoreFolder.trim() || [...restoreFolder.trim()].length > 128}>
                选择备份和恢复位置
              </button>
            </div>
          </form>
        )}
        {error && <p role="alert">{error}</p>}
        {workspace && <DatasetBrowser key={workspace.workspace_session_id} workspace={workspace}
          reviewBlocked={reviewBlocked} onReviewBlocked={setReviewBlocked} />}
        {workspace && <VideoImportPanel key={"video-" + workspace.workspace_session_id} workspace={workspace}
          editGuard={{ blocked: videoEditBlocked, onBlocked: setVideoEditBlocked }} />}
        {workspace && <WorkspaceBackupPanel key={"backup-" + workspace.workspace_session_id} workspace={workspace} />}
      </main>
    </div>
  );
}

const root = document.getElementById("root");
if (!root) throw new Error("Missing desktop root element");
createRoot(root).render(<App />);
