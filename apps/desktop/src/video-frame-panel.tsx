import { useEffect, useRef, useState } from "react";

import type { DisplayLocator, VideoFrame, VideoSeekPolicy } from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { getFrameImageLocator, resolveVideoFrame } from "./api/video";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const pendingStates = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);
const errors: Record<string, string> = {
  WORKER_START_FAILED: "视频 Worker 启动失败，请检查本机配置。",
  WORKER_INTERRUPTED: "帧解析已中断，可以重新请求。",
  INPUT_CHANGED: "视频内容已变化，请重新探测。",
  FILE_IO_FAILED: "视频源文件无法读取，请检查文件是否在线。",
  SESSION_ENDED: "原帧任务会话已结束，请重新请求。",
};

export function VideoFramePanel({ workspace, sourceVideoId, enabled }: {
  workspace: OpenWorkspace; sourceVideoId: string; enabled: boolean;
}) {
  const [mode, setMode] = useState<"time" | "index">("time");
  const [value, setValue] = useState("0");
  const [policy, setPolicy] = useState<VideoSeekPolicy>("NEAREST");
  const [busy, setBusy] = useState(false);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [frame, setFrame] = useState<VideoFrame | null>(null);
  const [image, setImage] = useState<DisplayLocator | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [queryRetry, setQueryRetry] = useState(0);
  const [imageRevision, setImageRevision] = useState(0);
  const key = useRef<string | null>(null);

  useEffect(() => {
    if (!taskId) return;
    const id = taskId;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const task = await getTask(workspace, id);
        if (!active) return;
        if (pendingStates.has(task.status)) {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setTaskId(null);
        key.current = null;
        if (task.status === "SUCCEEDED" && task.video_frame) {
          setFrame(task.video_frame);
        } else {
          setError(errors[task.error_code ?? ""] ?? "无法完成视频帧解析。");
        }
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [workspace, taskId, queryRetry]);

  useEffect(() => {
    let active = true;
    setImage(null);
    if (frame) {
      void getFrameImageLocator(workspace, frame).then((locator) => {
        if (active) setImage(locator);
      }).catch((failure: unknown) => {
        if (active) setError(workspaceCommandError(failure));
      });
    }
    return () => { active = false; };
  }, [workspace, frame, imageRevision]);

  const valid = /^(0|[1-9][0-9]*)$/.test(value) && value.length <= 20
    && BigInt(value) <= 18_446_744_073_709_551_615n;
  const disabled = busy || taskId !== null;

  async function submit() {
    if (!enabled || disabled || !valid) return;
    key.current ??= crypto.randomUUID();
    setBusy(true);
    setError(null);
    setFrame(null);
    try {
      const target = mode === "time" ? { timestamp_ms: value } : { frame_index: value };
      const result = await resolveVideoFrame(workspace, sourceVideoId, target, policy, key.current);
      if (result.kind === "ready") {
        setFrame(result.frame);
        key.current = null;
      } else {
        setTaskId(result.operation.task_id);
      }
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="video-frame" aria-label="精确帧定位">
      <form onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <label>定位方式 <select value={mode} disabled={disabled} onChange={(event) => {
          setMode(event.target.value as "time" | "index"); key.current = null;
        }}><option value="time">时间</option><option value="index">显示帧序号</option></select></label>
        <label>{mode === "time" ? "时间（毫秒）" : "显示帧序号（从 0 开始）"}
          <input inputMode="numeric" maxLength={20} value={value} disabled={disabled}
            onChange={(event) => { setValue(event.target.value); key.current = null; }} />
        </label>
        {mode === "time" && <label>定位规则 <select value={policy} disabled={disabled} onChange={(event) => {
          setPolicy(event.target.value as VideoSeekPolicy); key.current = null;
        }}><option value="NEAREST">最近一帧</option><option value="AT_OR_AFTER">该时间或之后</option></select></label>}
        <button type="submit" disabled={!enabled || disabled || !valid}>读取视频帧</button>
      </form>
      {!enabled && <p>视频来源可用且探测成功后可以读取帧。</p>}
      {(busy || taskId) && !error && <p role="status">正在解析视频帧…</p>}
      {taskId && error && <button type="button" onClick={() => {
        setError(null); setQueryRetry((revision) => revision + 1);
      }}>重试查询</button>}
      {error && <p role="alert">{error}</p>}
      {frame && <>
        <p>显示帧序号 {frame.frame_index}，实际时间 {frame.pts_us === null ? "未知" : frame.pts_us + " 微秒"}</p>
        {image && <img src={image.locator} width={frame.width} height={frame.height} alt={"视频帧 " + frame.frame_index}
          onError={() => setError("帧图像无法显示，请刷新帧显示。")} />}
        <button type="button" onClick={() => { setError(null); setImageRevision((revision) => revision + 1); }}>刷新帧显示</button>
      </>}
    </section>
  );
}
