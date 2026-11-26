import { useEffect, useRef, useState } from "react";
import type { StagingItem, StagingPage, StagingState } from "./api/generated/wire-primitives";
import { listStaging } from "./api/staging";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";

const labels: Record<StagingState, string> = {
  OFFLINE: "文件离线",
  CONFLICT: "存在冲突",
  ACCEPTING: "正在接收",
  CHECKING: "正在检查",
  NEEDS_ANNOTATION: "待标注",
  NEEDS_REVIEW: "待审核",
  NEEDS_CHECK: "待检查",
  READY_FOR_ACCEPTANCE: "可接收",
};

export function StagingPanel({ workspace, datasetId, refreshKey, selectionBlocked, onSelect }: {
  workspace: OpenWorkspace;
  datasetId: string;
  refreshKey: number;
  selectionBlocked: boolean;
  onSelect: (item: StagingItem) => void;
}) {
  const [status, setStatus] = useState<StagingState | undefined>();
  const [page, setPage] = useState<StagingPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const generation = useRef(0);

  useEffect(() => {
    const generationId = ++generation.current;
    setPage(null);
    setError(null);
    setBusy(true);
    void listStaging(workspace, datasetId, status).then(result => {
      if (generation.current === generationId) setPage(result);
    }).catch(failure => {
      if (generation.current === generationId) setError(workspaceCommandError(failure));
    }).finally(() => {
      if (generation.current === generationId) setBusy(false);
    });
    return () => { generation.current += 1; };
  }, [workspace, datasetId, status, revision, refreshKey]);

  async function more() {
    if (busy || !page?.next_cursor) return;
    const generationId = generation.current;
    setBusy(true);
    setError(null);
    try {
      const result = await listStaging(workspace, datasetId, status, page.next_cursor);
      if (generation.current === generationId) {
        setPage({ items: [...page.items, ...result.items], next_cursor: result.next_cursor });
      }
    } catch (failure) {
      if (generation.current === generationId) setError(workspaceCommandError(failure));
    } finally {
      if (generation.current === generationId) setBusy(false);
    }
  }

  return <section aria-label="待接收样本">
    <h4>待接收样本</h4>
    <label>状态 <select value={status ?? ""} onChange={event => setStatus(event.target.value as StagingState || undefined)}>
      <option value="">全部状态</option>
      {Object.entries(labels).map(([state, label]) => <option key={state} value={state}>{label}</option>)}
    </select></label>
    <button type="button" disabled={busy} onClick={() => setRevision(value => value + 1)}>刷新待接收区</button>
    {busy && <p role="status">正在核验待接收样本…</p>}
    {error && <p role="alert">{error}</p>}
    {page?.items.length === 0 && !page.next_cursor && <p>当前没有符合条件的待接收样本。</p>}
    {page && page.items.length > 0 && <table>
      <thead><tr><th scope="col">文件名</th><th scope="col">状态</th><th scope="col">来源帧</th></tr></thead>
      <tbody>{page.items.map(item => <tr key={item.asset_id}>
        <td><button type="button" className="asset-link"
          disabled={selectionBlocked || item.state === "OFFLINE" || item.target_dataset_id !== datasetId}
          onClick={() => onSelect(item)}>{item.display_name}</button></td>
        <td>{labels[item.state]}</td>
        <td>{item.source_frame_ref
          ? "帧 " + item.source_frame_ref.frame_index + (item.source_frame_ref.pts_us === null ? "" : " · " + item.source_frame_ref.pts_us + " µs")
          : "—"}</td>
      </tr>)}</tbody>
    </table>}
    {page?.next_cursor && <button type="button" disabled={busy} onClick={() => void more()}>更多待接收样本</button>}
  </section>;
}
