import { useEffect, useState } from "react";
import type { AnnotationState, AssetPage, DatasetPage, DatasetSummary } from "./api/generated/wire-primitives";
import { listAssets, listDatasets } from "./api/dataset";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const annotationLabels: Record<AnnotationState, string> = {
  UNANNOTATED: "无标注",
  UNREVIEWED: "待审核",
  CONFIRMED_POSITIVE: "已确认",
  CONFIRMED_NEGATIVE: "已确认负样本",
  STALE: "确认已失效",
};

export function DatasetBrowser({ workspace }: { workspace: OpenWorkspace }) {
  const [page, setPage] = useState<DatasetPage | null>(null);
  const [selected, setSelected] = useState<DatasetSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  useEffect(() => {
    let current = true;
    listDatasets(workspace).then((loaded) => {
      if (current) setPage(loaded);
    }).catch((failure: unknown) => {
      if (current) setError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace]);

  async function more() {
    if (!page?.next_cursor) return;
    setLoadingMore(true);
    setError(null);
    try {
      const loaded = await listDatasets(workspace, page.next_cursor);
      setPage({ items: [...page.items, ...loaded.items], next_cursor: loaded.next_cursor });
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setLoadingMore(false);
    }
  }

  return (
    <section className="dataset-browser" aria-labelledby="dataset-heading">
      <h2 id="dataset-heading">数据集</h2>
      {error && <p role="alert">{error}</p>}
      {!page && !error && <p role="status">正在读取数据集…</p>}
      {page?.items.length === 0 && <p>此工作区还没有数据集。</p>}
      {page && page.items.length > 0 && (
        <div className="dataset-layout">
          <div>
            <ul className="dataset-list">
              {page.items.map((dataset) => (
                <li key={dataset.dataset_id}>
                  <button type="button" aria-pressed={selected?.dataset_id === dataset.dataset_id} onClick={() => setSelected(dataset)}>
                    <span>{dataset.name}</span>
                    <small>{dataset.asset_count} 项资源</small>
                  </button>
                </li>
              ))}
            </ul>
            {page.next_cursor && <button type="button" disabled={loadingMore} onClick={() => void more()}>更多数据集</button>}
          </div>
          {selected && <AssetTable key={selected.dataset_id} workspace={workspace} dataset={selected} />}
        </div>
      )}
    </section>
  );
}

function AssetTable({ workspace, dataset }: { workspace: OpenWorkspace; dataset: DatasetSummary }) {
  const [page, setPage] = useState<AssetPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  useEffect(() => {
    let current = true;
    listAssets(workspace, dataset.dataset_id).then((loaded) => {
      if (current) setPage(loaded);
    }).catch((failure: unknown) => {
      if (current) setError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace, dataset.dataset_id]);

  async function more() {
    if (!page?.next_cursor) return;
    setLoadingMore(true);
    setError(null);
    try {
      const loaded = await listAssets(workspace, dataset.dataset_id, page.next_cursor);
      setPage({ items: [...page.items, ...loaded.items], next_cursor: loaded.next_cursor });
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setLoadingMore(false);
    }
  }

  return (
    <div className="dataset-assets">
      <h3>{dataset.name}</h3>
      {error && <p role="alert">{error}</p>}
      {!page && !error && <p role="status">正在读取资源…</p>}
      {page?.items.length === 0 && <p>此数据集没有资源。</p>}
      {page && page.items.length > 0 && (
        <table>
          <thead><tr><th scope="col">文件名</th><th scope="col">标注状态</th></tr></thead>
          <tbody>
            {page.items.map((asset) => (
              <tr key={asset.asset_id}>
                <td>{asset.display_name}</td>
                <td>{asset.availability === "OFFLINE" ? "文件离线" : annotationLabels[asset.annotation_state]}
                  {asset.active_review && <small> · 审核进行中</small>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {page?.next_cursor && <button type="button" disabled={loadingMore} onClick={() => void more()}>更多资源</button>}
    </div>
  );
}
