import { useEffect, useRef, useState } from "react";
import type { StagingItem, StagingPage, StagingState } from "./api/generated/wire-primitives";
import { listStaging } from "./api/staging";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { StagingBatchReportPanel } from "./staging-batch-report-panel";
import { StagingReportPanel } from "./staging-report-panel";

const maxBatchAssets = 1000;

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

export function StagingPanel({ workspace, datasetId, refreshKey, selectionBlocked, onSelect, onAccepted }: {
  workspace: OpenWorkspace;
  datasetId: string;
  refreshKey: number;
  selectionBlocked: boolean;
  onSelect: (item: StagingItem) => void;
  onAccepted: () => void;
}) {
  const [status, setStatus] = useState<StagingState | undefined>();
  const [page, setPage] = useState<StagingPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const [reportAsset, setReportAsset] = useState<string | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [batchItems, setBatchItems] = useState<StagingItem[] | null>(null);
  const generation = useRef(0);

  useEffect(() => {
    const generationId = ++generation.current;
    setPage(null);
    setError(null);
    setBusy(true);
    setReportAsset(null);
    setSelected(new Set());
    setBatchItems(null);
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

  const reportItem = page?.items.find(item => item.asset_id === reportAsset && item.current_report_ref);
  const batchOpen = batchItems !== null;

  function prepareBatch() {
    const items = page?.items.filter(item => selected.has(item.asset_id)) ?? [];
    if (items.length < 2 || selectionBlocked || busy) return;
    setReportAsset(null);
    setBatchItems(items);
  }

  return <section aria-label="待接收样本">
    <h4>待接收样本</h4>
    <label>状态 <select disabled={batchOpen} value={status ?? ""}
      onChange={event => setStatus(event.target.value as StagingState || undefined)}>
      <option value="">全部状态</option>
      {Object.entries(labels).map(([state, label]) => <option key={state} value={state}>{label}</option>)}
    </select></label>
    <button type="button" disabled={busy || batchOpen} onClick={() => setRevision(value => value + 1)}>刷新待接收区</button>
    {busy && <p role="status">正在核验待接收样本…</p>}
    {error && <p role="alert">{error}</p>}
    {page?.items.length === 0 && !page.next_cursor && <p>当前没有符合条件的待接收样本。</p>}
    {page && page.items.length > 0 && <div className="review-actions">
      <button type="button" data-staging-batch-build disabled={busy || batchOpen || selectionBlocked || selected.size < 2}
        onClick={prepareBatch}>准备批次接收{selected.size > 0 ? "（" + selected.size + "）" : ""}</button>
      {selected.size === maxBatchAssets && <span>每批最多接收 1000 个样本。</span>}
    </div>}
    {page && page.items.length > 0 && <table>
      <thead><tr><th scope="col">批选</th><th scope="col">文件名</th><th scope="col">状态</th><th scope="col">来源帧</th><th scope="col">检查报告</th></tr></thead>
      <tbody>{page.items.map(item => <tr key={item.asset_id}>
        <td>{item.state === "READY_FOR_ACCEPTANCE" && item.current_report_ref && item.target_dataset_id === datasetId &&
          <input type="checkbox" data-staging-batch-asset={item.asset_id} aria-label={"批选 " + item.display_name}
            checked={selected.has(item.asset_id)}
            disabled={busy || batchOpen || selectionBlocked || (!selected.has(item.asset_id) && selected.size >= maxBatchAssets)}
            onChange={event => {
              const checked = event.target.checked;
              setSelected(previous => {
                const next = new Set(previous);
                if (checked && next.size < maxBatchAssets) next.add(item.asset_id);
                if (!checked) next.delete(item.asset_id);
                return next;
              });
            }} />}</td>
        <td><button type="button" className="asset-link"
          disabled={batchOpen || selectionBlocked || item.state === "OFFLINE" || item.target_dataset_id !== datasetId}
          onClick={() => onSelect(item)}>{item.display_name}</button></td>
        <td>{labels[item.state]}</td>
        <td>{item.source_frame_ref
          ? "帧 " + item.source_frame_ref.frame_index + (item.source_frame_ref.pts_us === null ? "" : " · " + item.source_frame_ref.pts_us + " µs")
          : "—"}</td>
        <td>{item.current_report_ref && <button type="button" disabled={batchOpen}
          onClick={() => setReportAsset(item.asset_id)}>查看报告</button>}</td>
      </tr>)}</tbody>
    </table>}
    {page?.next_cursor && <button type="button" disabled={busy || batchOpen} onClick={() => void more()}>更多待接收样本</button>}
    {batchItems && <StagingBatchReportPanel workspace={workspace} datasetId={datasetId} items={batchItems}
      acceptanceBlocked={selectionBlocked} onClose={() => setBatchItems(null)} onAccepted={() => {
        setBatchItems(null);
        setRevision(value => value + 1);
        onAccepted();
      }} />}
    {reportItem && <StagingReportPanel key={reportItem.current_report_ref?.artifact_id}
      workspace={workspace} item={reportItem} acceptanceBlocked={selectionBlocked}
      onClose={() => setReportAsset(null)} onAccepted={() => {
        setReportAsset(null);
        onAccepted();
      }} />}
  </section>;
}
