import { useEffect, useRef, useState } from "react";

import type { AssetPage } from "./api/generated/wire-primitives";
import { listVideos } from "./api/video";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";
import { VideoDetailPanel } from "./video-detail-panel";

export function VideoLibraryPanel({ workspace, refreshKey }: {
  workspace: OpenWorkspace; refreshKey: string;
}) {
  const [page, setPage] = useState<AssetPage | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const generation = useRef(0);

  useEffect(() => {
    let active = true;
    generation.current += 1;
    setBusy(true);
    setPage(null);
    setSelected(null);
    setError(null);
    void listVideos(workspace).then((loaded) => {
      if (active) setPage(loaded);
    }).catch((failure: unknown) => {
      if (active) setError(workspaceCommandError(failure));
    }).finally(() => { if (active) setBusy(false); });
    return () => { active = false; generation.current += 1; };
  }, [workspace, refreshKey, revision]);

  async function more() {
    if (!page?.next_cursor || busy) return;
    const requestGeneration = generation.current;
    setBusy(true);
    setError(null);
    try {
      const loaded = await listVideos(workspace, page.next_cursor);
      if (requestGeneration === generation.current) {
        setPage({ items: [...page.items, ...loaded.items], next_cursor: loaded.next_cursor });
      }
    } catch (failure) {
      if (requestGeneration === generation.current) setError(workspaceCommandError(failure));
    } finally {
      if (requestGeneration === generation.current) setBusy(false);
    }
  }

  return (
    <section aria-label="工作区视频">
      <h2>已登记视频</h2>
      <button type="button" disabled={busy} onClick={() => setRevision((value) => value + 1)}>刷新列表</button>
      {busy && <p role="status">正在读取视频列表…</p>}
      {error && <p role="alert">{error}</p>}
      {page?.items.length === 0 && <p>当前工作区尚未登记视频。</p>}
      <ul>{page?.items.map((video) => <li key={video.asset_id}>
        <button type="button" aria-pressed={selected === video.asset_id}
          onClick={() => setSelected(video.asset_id)}>{video.display_name}</button>
      </li>)}</ul>
      {page?.next_cursor && <button type="button" disabled={busy} onClick={() => void more()}>加载更多视频</button>}
      {selected && <VideoDetailPanel key={selected} workspace={workspace} sourceVideoId={selected} />}
    </section>
  );
}
