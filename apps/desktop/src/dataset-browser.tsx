import { useCallback, useEffect, useState } from "react";
import { DatasetImportPanel } from "./dataset-import-panel";
import { GeometryCheckPanel } from "./geometry-check-panel";
import type { GeometryEvidence } from "./geometry-check-panel";
import { ReviewPanel } from "./review-panel";
import type { AnnotationState, AnnotationView, AssetPage, AssetSummary, DatasetPage, DatasetSummary, DisplayLocator } from "./api/generated/wire-primitives";
import { getAnnotation, getDisplayLocator, listAssets, listDatasets } from "./api/dataset";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const annotationLabels: Record<AnnotationState, string> = {
  UNANNOTATED: "无标注",
  UNREVIEWED: "待审核",
  CONFIRMED_POSITIVE: "已确认",
  CONFIRMED_NEGATIVE: "已确认负样本",
  STALE: "确认已失效",
};

type ReviewGuard = { reviewBlocked: boolean; onReviewBlocked: (blocked: boolean) => void };

export function DatasetBrowser({ workspace, reviewBlocked, onReviewBlocked }: { workspace: OpenWorkspace } & ReviewGuard) {
  const [page, setPage] = useState<DatasetPage | null>(null);
  const [selected, setSelected] = useState<DatasetSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const refreshDatasets = useCallback(() => setRefresh((value) => value + 1), []);

  useEffect(() => {
    let current = true;
    listDatasets(workspace).then((loaded) => {
      if (current) setPage(loaded);
    }).catch((failure: unknown) => {
      if (current) setError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace, refresh]);

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
      <DatasetImportPanel workspace={workspace} onImported={refreshDatasets} />
      {error && <p role="alert">{error}</p>}
      {!page && !error && <p role="status">正在读取数据集…</p>}
      {page?.items.length === 0 && <p>此工作区还没有数据集。</p>}
      {page && page.items.length > 0 && (
        <div className="dataset-layout">
          <div>
            <ul className="dataset-list">
              {page.items.map((dataset) => (
                <li key={dataset.dataset_id}>
                  <button type="button" disabled={reviewBlocked} aria-pressed={selected?.dataset_id === dataset.dataset_id} onClick={() => setSelected(dataset)}>
                    <span>{dataset.name}</span>
                    <small>{dataset.asset_count} 项资源</small>
                  </button>
                </li>
              ))}
            </ul>
            {page.next_cursor && <button type="button" disabled={loadingMore} onClick={() => void more()}>更多数据集</button>}
          </div>
          {selected && <AssetTable key={selected.dataset_id} workspace={workspace} dataset={selected}
            reviewBlocked={reviewBlocked} onReviewBlocked={onReviewBlocked} />}
        </div>
      )}
    </section>
  );
}

function AssetTable({ workspace, dataset, reviewBlocked, onReviewBlocked }: {
  workspace: OpenWorkspace; dataset: DatasetSummary;
} & ReviewGuard) {
  const [selected, setSelected] = useState<AssetSummary | null>(null);
  const [page, setPage] = useState<AssetPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const refreshAssets = useCallback(() => setRefresh((value) => value + 1), []);

  useEffect(() => {
    let current = true;
    listAssets(workspace, dataset.dataset_id).then((loaded) => {
      if (current) setPage(loaded);
    }).catch((failure: unknown) => {
      if (current) setError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace, dataset.dataset_id, refresh]);

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
                <td><button type="button" className="asset-link" disabled={reviewBlocked} onClick={() => setSelected(asset)}>{asset.display_name}</button></td>
                <td>{asset.availability === "OFFLINE" ? "文件离线" : annotationLabels[asset.annotation_state]}
                  {asset.active_review && <small> · 审核进行中</small>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {page?.next_cursor && <button type="button" disabled={loadingMore} onClick={() => void more()}>更多资源</button>}
      {selected && <AnnotationDetails key={selected.asset_id} workspace={workspace} asset={selected}
        reviewBlocked={reviewBlocked} onReviewBlocked={onReviewBlocked} onChanged={refreshAssets} />}
    </div>
  );
}

function AnnotationDetails({ workspace, asset, reviewBlocked, onReviewBlocked, onChanged }: {
  workspace: OpenWorkspace; asset: AssetSummary; onChanged: () => void;
} & ReviewGuard) {
  const [media, setMedia] = useState<DisplayLocator | null>(null);
  const [evidence, setEvidence] = useState<GeometryEvidence | null>(null);
  const [imageError, setImageError] = useState<string | null>(null);
  const [mediaRefresh, setMediaRefresh] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const [view, setView] = useState<AnnotationView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const refreshAnnotation = useCallback(() => { setRefresh((value) => value + 1); onChanged(); }, [onChanged]);
  useEffect(() => {
    let current = true;
    setMedia(null);
    setImageError(null);
    getDisplayLocator(workspace, asset.asset_id).then((loaded) => {
      if (current) setMedia(loaded);
    }).catch((failure: unknown) => {
      if (current) setImageError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace, asset.asset_id, refresh, mediaRefresh]);

  useEffect(() => {
    let current = true;
    setView(null);
    setError(null);
    getAnnotation(workspace, asset.asset_id).then((loaded) => {
      if (current) setView(loaded);
    }).catch((failure: unknown) => {
      if (current) setError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace, asset.asset_id, refresh]);

  function markImageError() {
    setImageError("图像显示授权已失效或文件无法解码。");
  }

  const document = view?.formal_coco;
  return (
    <section className="annotation-details" aria-label={asset.display_name + " 的标注"}>
      <h4>{asset.display_name}</h4>
      <button type="button" disabled={reviewBlocked} onClick={() => setRefresh((value) => value + 1)}>刷新样本</button>
      {media && !imageError && <img className="sample-image" src={media.locator} alt={asset.display_name} onError={markImageError} />}
      {imageError && <p role="alert">{imageError} <button type="button" onClick={() => setMediaRefresh((value) => value + 1)}>重新读取图像</button></p>}
      {error && <p role="alert">{error}</p>}
      {!view && !error && <p role="status">正在读取标注…</p>}
      {view && <p>{annotationLabels[view.annotation_state]}{view.active_review ? " · 审核进行中" : ""}</p>}
      {view && !document && <p>此样本没有标注文件。</p>}
      <GeometryCheckPanel workspace={workspace} asset={asset} onEvidence={setEvidence} />
      {view && <ReviewPanel workspace={workspace} asset={asset} annotation={view} media={imageError ? null : media}
        evidence={evidence} onBlocked={onReviewBlocked} onImageError={markImageError}
        onChanged={refreshAnnotation} />}
      {document && (
        <>
          <p>标注尺寸 {document.images[0].width} × {document.images[0].height} 像素</p>
          {document.annotations.length === 0 ? <p>标注文件中没有框。</p> : (
            <table>
              <thead><tr><th scope="col">类别</th><th scope="col">框坐标 x、y、宽、高</th></tr></thead>
              <tbody>{document.annotations.map((box) => (
                <tr key={box.id}>
                  <td>{document.categories.find((category) => category.id === box.category_id)?.name ?? ("未知类别 " + box.category_id)}</td>
                  <td>{box.bbox.join(", ")}</td>
                </tr>
              ))}</tbody>
            </table>
          )}
        </>
      )}
    </section>
  );
}
