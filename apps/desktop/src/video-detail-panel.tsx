import { useEffect, useState } from "react";
import { VideoFramePanel } from "./video-frame-panel";
import type { VideoEditGuard } from "./video-annotation-panel";

import type { VideoDetail } from "./api/generated/wire-primitives";
import { getVideo } from "./api/video";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

export function VideoDetailPanel({ workspace, sourceVideoId, editGuard, importRunning }: {
  workspace: OpenWorkspace; sourceVideoId: string; editGuard: VideoEditGuard; importRunning: boolean;
}) {
  const [detail, setDetail] = useState<VideoDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let active = true;
    setBusy(true);
    setError(null);
    void getVideo(workspace, sourceVideoId).then((loaded) => {
      if (active) setDetail(loaded);
    }).catch((failure: unknown) => {
      if (active) setError(workspaceCommandError(failure));
    }).finally(() => { if (active) setBusy(false); });
    return () => { active = false; };
  }, [workspace, sourceVideoId, revision]);

  return (
    <section aria-label="视频信息">
      <button type="button" disabled={busy || editGuard.blocked} onClick={() => setRevision((value) => value + 1)}>刷新视频信息</button>
      {busy && <p role="status">正在核验视频来源…</p>}
      {error && <p role="alert">{error}</p>}
      {detail && <>
        <h2>{detail.display_name}</h2>
        <p>来源状态 {detail.source_availability === "ONLINE" ? "可用"
          : detail.source_availability === "OFFLINE" ? "离线" : "内容已变化"}</p>
        {detail.duration_ms !== null && <p>时长 {(Number(detail.duration_ms) / 1000).toLocaleString()} 秒</p>}
        <p>可变帧率 {detail.variable_frame_rate === null ? "未知" : detail.variable_frame_rate ? "是" : "否"}</p>
        {detail.rotation_ccw !== null && <p>逆时针旋转 {detail.rotation_ccw}°</p>}
        {detail.streams.length === 0 && <p>当前没有已核验的视频流信息。</p>}
        {detail.streams.length > 0 && <table>
          <thead><tr><th>视频流</th><th>编码</th><th>尺寸</th><th>时长</th></tr></thead>
          <tbody>{detail.streams.map((stream) => <tr key={stream.stream_index}>
            <td>{stream.stream_index}</td>
            <td>{stream.codec ?? "未知"}</td>
            <td>{stream.width !== null && stream.height !== null ? `${stream.width} × ${stream.height}` : "未知"}</td>
            <td>{stream.duration_ms === null ? "未知" : `${Number(stream.duration_ms) / 1000} 秒`}</td>
          </tr>)}</tbody>
        </table>}
        <VideoFramePanel key={detail.source_video_id + detail.source_digest} workspace={workspace}
          detail={detail} enabled={!error && detail.source_availability === "ONLINE" && detail.probe_status === "SUCCEEDED"}
          verifying={busy || importRunning} editGuard={editGuard}
          onAnnotationsChanged={() => setRevision((value) => value + 1)} />
      </>}
    </section>
  );
}
