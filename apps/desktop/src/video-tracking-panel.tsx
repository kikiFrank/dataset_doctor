import { useEffect, useRef, useState } from "react";
import type {
  CocoCategory, DatasetSamplingMode, VideoFrame, VideoSegment, VideoTrackingView,
} from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { TaskCancelButton } from "./task-cancel-button";
import { submitVideoTracking } from "./api/video";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { matchingTrackingReceipt, trackingObservationsAtFrame, trackingSamplingConfig } from "./video-annotation";

const active = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);
const errors: Record<string, string> = {
  INPUT_CHANGED: "视频来源或数据集类别已变化，请重新核对。",
  input_changed: "视频来源或冻结的帧信息已变化，请重新核对。",
  WORKER_START_FAILED: "追踪 Worker 无法启动，请检查本机视频工具。",
  WORKER_INTERRUPTED: "追踪进程已中断。",
  SESSION_ENDED: "原工作区会话已结束，追踪已中断。",
  INPUT_MANIFEST_INVALID: "无法核验追踪输入清单。",
};
const reasons: Record<string, string> = {
  UNSUPPORTED_SEED: "种子纹理不足或超出适配器范围",
  TARGET_LOST: "目标丢失",
  AMBIGUOUS_MATCH: "有多个相近匹配",
  AMBIGUOUS_IDENTITY: "多个轨迹匹配同一区域",
};

export function VideoTrackingPanel({ workspace, segments, frame, categories, schema, receipt, onReceipt, disabled }: {
  workspace: OpenWorkspace;
  segments: VideoSegment[];
  frame: VideoFrame | null;
  categories: CocoCategory[];
  schema: string;
  receipt: VideoTrackingView | null;
  onReceipt: (receipt: VideoTrackingView | null) => void;
  disabled: boolean;
}) {
  const [segmentId, setSegmentId] = useState("");
  const [mode, setMode] = useState<DatasetSamplingMode>("EVERY_N_FRAMES");
  const [interval, setIntervalValue] = useState("1");
  const [busy, setBusy] = useState(false);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [taskStatus, setTaskStatus] = useState("QUEUED");
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const request = useRef({ fingerprint: "", key: "" });
  const segment = segments.find(item => item.segment_id === segmentId);

  useEffect(() => {
    if (!taskId) return;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const id = taskId;
    async function read() {
      try {
        const task = await getTask(workspace, id);
        if (!current) return;
        setTaskStatus(task.status);
        if (active.has(task.status)) {
          timer = setTimeout(() => void read(), 500);
          return;
        }
        setTaskId(null);
        request.current = { fingerprint: "", key: "" };
        if (task.status === "SUCCEEDED" && task.video_tracking) onReceipt(task.video_tracking);
        else setError(task.status === "CANCELLED" ? "追踪已取消，可以重新提交。" : errors[task.error_code ?? ""] ?? "追踪未完成，请核对片段和视频来源。");
      } catch (failure) {
        if (current) setError(workspaceCommandError(failure));
      }
    }
    void read();
    return () => { current = false; clearTimeout(timer); };
  }, [workspace, taskId, retry, onReceipt]);

  async function submit() {
    if (!segment || busy || disabled || taskId) return;
    setBusy(true);
    onReceipt(null);
    setError(null);
    try {
      const fields = {
        source_video_id: segment.source_video_id, target_dataset_id: segment.target_dataset_id,
        segment_id: segment.segment_id, tracking_sampling_config: trackingSamplingConfig(mode, interval),
      };
      const fingerprint = JSON.stringify([fields, segment.revision, schema]);
      if (request.current.fingerprint !== fingerprint) {
        request.current = { fingerprint, key: crypto.randomUUID() };
      }
      const operation = await submitVideoTracking(workspace, fields, request.current.key);
      setTaskId(operation.task_id);
      setTaskStatus(operation.status);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  const current = matchingTrackingReceipt(receipt, segment, schema);
  const observations = current ? trackingObservationsAtFrame(current, frame) : [];

  return <section aria-label="视频追踪">
    <h3>视频追踪</h3>
    <p>本机平移追踪使用首个有标注的关键帧，每次最多处理 512 个显示帧。</p>
    <form onSubmit={event => { event.preventDefault(); void submit(); }}>
      <fieldset disabled={disabled || busy || taskId !== null}>
        <label>已保存片段 <select value={segmentId} onChange={event => setSegmentId(event.target.value)}>
          <option value="">选择片段</option>
          {segments.map(item => <option key={item.segment_id} value={item.segment_id}>
            {item.start_us}–{item.end_us} µs · {item.keyframes.length} 个关键帧
          </option>)}
        </select></label>
        <label>追踪取帧方式 <select value={mode} onChange={event => {
          const selected = event.target.value as DatasetSamplingMode;
          setMode(selected);
          setIntervalValue(selected === "EVERY_N_FRAMES" ? "1" : "1000");
        }}>
          <option value="EVERY_N_FRAMES">每 N 帧</option>
          <option value="TIME_INTERVAL">固定时间间隔</option>
        </select></label>
        <label>{mode === "EVERY_N_FRAMES" ? "帧间隔" : "间隔（毫秒）"}
          <input inputMode="numeric" maxLength={7} value={interval} onChange={event => setIntervalValue(event.target.value)} />
        </label>
        <button type="submit" disabled={!segment || segment.keyframes.length === 0}>运行追踪</button>
      </fieldset>
    </form>
    {taskId && !error && <p role="status">正在对选定实际帧运行追踪…</p>}
    {taskId && <TaskCancelButton key={taskId} workspace={workspace} taskId={taskId} status={taskStatus} />}
    {error && <p role="alert">{error}</p>}
    {taskId && error && <button type="button" onClick={() => { setError(null); setRetry(value => value + 1); }}>重试查询</button>}
    {receipt && !current && <p>当前片段或类别与已保存结果的范围不一致，请重新追踪。</p>}
    {current && <>
      <p role="status">已处理 {current.processed_frame_count} 帧，{current.uncertain_ranges.length} 个轨迹区间需审核。</p>
      {frame && observations.length === 0 && <p>当前帧没有追踪结果。</p>}
      {observations.length > 0 && <table aria-label="当前帧的追踪结果">
        <thead><tr><th scope="col">轨迹</th><th scope="col">类别</th><th scope="col">来源</th><th scope="col">框坐标 x、y、宽、高</th></tr></thead>
        <tbody>{observations.map(item => <tr key={item.track_id}>
          <td title={item.track_id}>{item.track_id.slice(0, 8)}</td>
          <td>{categories.find(category => String(category.id) === item.category_id)?.name ?? item.category_id}</td>
          <td>{item.source === "HUMAN_SEED" ? "人工种子" : "追踪候选"}</td>
          <td>{item.bbox_xywh.join(", ")}</td>
        </tr>)}</tbody>
      </table>}
      {current.uncertain_ranges.length > 0 && <details>
        <summary>待审核区间</summary>
        <ul>{current.uncertain_ranges.map(span => <li key={span.track_id}>
          {span.track_id.slice(0, 8)} · 帧 {span.first_frame_index}–{span.last_frame_index}
          {" · "}{reasons[span.reason] ?? span.reason}
        </li>)}</ul>
      </details>}
    </>}
  </section>;
}
