import { useEffect, useRef, useState } from "react";
import { VideoDetailPanel } from "./video-detail-panel";
import type { PathGrant, VideoImportView } from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { importVideo, pickVideoFile } from "./api/video";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const errors: Record<string, string> = {
  WORKER_START_FAILED: "视频 Worker 启动失败，请检查本机视频工具配置。",
  WORKER_INTERRUPTED: "视频探测中断。",
  INPUT_CHANGED: "视频源文件发生变化，请重新选择文件。",
  DECODE_FAILED: "无法解码该视频。",
  INVALID_INPUT: "当前视频无法自动确定视频流，或超过探测范围。",
};

const pending = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);

export function VideoImportPanel({ workspace }: { workspace: OpenWorkspace }) {
  const [selection, setSelection] = useState<PathGrant | null>(null);
  const [receipt, setReceipt] = useState<VideoImportView | null>(null);
  const [taskStatus, setTaskStatus] = useState("");
  const [streamIndex, setStreamIndex] = useState("");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [queryRevision, setQueryRevision] = useState(0);
  const idempotencyKey = useRef(crypto.randomUUID());

  useEffect(() => {
    if (!receipt) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const operation = receipt.operation;
    async function poll() {
      try {
        const task = await getTask(workspace, operation.task_id);
        if (!active) return;
        if (pending.has(task.status)) {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setTaskStatus(task.status);
        setStatus(task.status === "SUCCEEDED" ? "视频已登记，帧索引可用。" : "");
        if (task.status !== "SUCCEEDED") setError(errors[task.error_code ?? ""] ?? "视频探测未完成。");
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [receipt, workspace, queryRevision]);

  async function choose() {
    setBusy(true);
    try {
      const selected = await pickVideoFile(workspace);
      if (!selected) return;
      idempotencyKey.current = crypto.randomUUID();
      setSelection(selected);
      setReceipt(null);
      setTaskStatus("");
      setStreamIndex("");
      setError(null);
      setStatus("");
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function submit() {
    if (!selection || !validStream) return;
    setBusy(true);
    setError(null);
    try {
      const created = await importVideo(workspace, selection.grant_id, idempotencyKey.current,
        streamIndex === "" ? null : Number(streamIndex));
      setReceipt(created);
      setTaskStatus(created.operation.status);
      setStatus(created.operation.status === "SUCCEEDED" ? "视频已登记，帧索引可用。" : "");
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  const running = pending.has(taskStatus);
  const validStream = streamIndex === "" || (/^\d+$/.test(streamIndex) && Number(streamIndex) <= 4_294_967_295);
  return (
    <section className="video-import" aria-label="视频导入">
      <button type="button" disabled={busy || running} onClick={() => void choose()}>选择视频</button>
      {selection && <>
        <p className="import-path">{selection.display_path}</p>
        <label htmlFor="video-stream">视频流编号（可选）</label>
        <input id="video-stream" inputMode="numeric" value={streamIndex} disabled={busy || receipt !== null}
          onChange={(event) => {
            idempotencyKey.current = crypto.randomUUID();
            setStreamIndex(event.target.value);
          }} />
        <button type="button" disabled={busy || receipt !== null || !validStream}
          onClick={() => void submit()}>登记并探测</button>
      </>}
      {running && !error && <p role="status">正在探测视频帧…</p>}
      {status && <p role="status">{status}</p>}
      {receipt && !running && <VideoDetailPanel key={receipt.source_video_id} workspace={workspace} sourceVideoId={receipt.source_video_id} />}
      {error && <p role="alert">{error}</p>}
      {running && error && <button type="button" onClick={() => {
        setError(null);
        setQueryRevision((value) => value + 1);
      }}>重新查询任务</button>}
    </section>
  );
}
