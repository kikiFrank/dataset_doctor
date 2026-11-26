import { useEffect, useRef, useState } from "react";
import type {
  DatasetSamplingMode, VideoSampleView, VideoSegment, VideoTrackingView,
} from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { submitVideoSamples } from "./api/video";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { datasetSamplingConfig, matchingTrackingReceipt } from "./video-annotation";

const active = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);
const failureMessage: Record<string, string> = {
  INPUT_CHANGED: "视频来源或目标类别已变化，请重新核对再采样。",
  WORKER_START_FAILED: "视频 Worker 无法启动，请检查本机配置。",
  SESSION_ENDED: "原工作区会话已关闭，采样已中断。",
  INPUT_MANIFEST_INVALID: "无法核验本次采样的输入清单。",
  FILE_IO_FAILED: "视频源文件已不可读。",
  STAGING_PUBLICATION_FAILED: "待接收样本登记失败，请检查磁盘及工作区状态。",
  TRACKING_EVIDENCE_INVALID: "无法核验追踪结果，请重新追踪后再采样。",
};

export function VideoSamplingPanel({ workspace, segments, tracks, schema, disabled }: {
  workspace: OpenWorkspace;
  segments: VideoSegment[];
  tracks: VideoTrackingView | null;
  schema: string;
  disabled: boolean;
}) {
  const [segmentId, setSegmentId] = useState("");
  const [mode, setMode] = useState<DatasetSamplingMode>("TIME_INTERVAL");
  const [interval, setIntervalValue] = useState("1000");
  const [maximum, setMaximum] = useState("");
  const [includeKeyframes, setIncludeKeyframes] = useState(false);
  const [selectedTracks, setSelectedTracks] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<VideoSampleView | null>(null);
  const [retry, setRetry] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const request = useRef({ fingerprint: "", key: "" });
  const segment = segments.find(item => item.segment_id === segmentId);
  const trackRef = matchingTrackingReceipt(tracks, segment, schema)?.tracks_ref;
  const includeTracks = trackRef !== undefined && selectedTracks === trackRef.artifact_id;

  useEffect(() => {
    if (!taskId) return;
    const id = taskId;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function read() {
      try {
        const result = await getTask(workspace, id);
        if (!current) return;
        if (active.has(result.status)) {
          timer = setTimeout(() => void read(), 500);
          return;
        }
        setTaskId(null);
        request.current = { fingerprint: "", key: "" };
        if (result.status === "SUCCEEDED" && result.video_sample) {
          setReceipt(result.video_sample);
        } else {
          setError(failureMessage[result.error_code ?? ""] ?? "采样未完成，请核对任务和视频来源。");
        }
      } catch (failure) {
        if (current) setError(workspaceCommandError(failure));
      }
    }
    void read();
    return () => { current = false; clearTimeout(timer); };
  }, [workspace, taskId, retry]);

  async function submit() {
    if (!segment || busy || disabled || taskId) return;
    setError(null);
    setReceipt(null);
    setBusy(true);
    try {
      const config = datasetSamplingConfig(mode, interval, includeKeyframes, maximum);
      const fields = {
        segment_id: segment.segment_id, source_video_id: segment.source_video_id,
        target_dataset_id: segment.target_dataset_id, dataset_sampling_config: config,
        ...(includeTracks ? { tracking_result_ref: trackRef } : {}),
      };
      const fingerprint = JSON.stringify([fields, segment.revision]);
      if (request.current.fingerprint !== fingerprint) {
        request.current = { fingerprint, key: crypto.randomUUID() };
      }
      const accepted = await submitVideoSamples(workspace, fields, request.current.key);
      setTaskId(accepted.task_id);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return <section aria-label="训练采样">
    <h3>训练采样</h3>
    <form onSubmit={event => { event.preventDefault(); void submit(); }}>
      <fieldset disabled={disabled || busy || Boolean(taskId)}>
        <label>已保存片段 <select value={segmentId} onChange={event => {
          setSelectedTracks(null);
          setSegmentId(event.target.value);
        }}>
          <option value="">选择片段</option>
          {segments.map(item => <option key={item.segment_id} value={item.segment_id}>
            {item.start_us}–{item.end_us} µs · {item.keyframes.length} 个关键帧
          </option>)}
        </select></label>
        <label>采样方式 <select value={mode} onChange={event => {
          const selected = event.target.value as DatasetSamplingMode;
          setMode(selected);
          setIntervalValue(selected === "TIME_INTERVAL" ? "1000" : "30");
        }}>
          <option value="TIME_INTERVAL">固定时间间隔</option>
          <option value="EVERY_N_FRAMES">每 N 帧</option>
        </select></label>
        <label>{mode === "TIME_INTERVAL" ? "间隔（毫秒）" : "帧间隔"}
          <input inputMode="numeric" maxLength={7} value={interval} onChange={event => setIntervalValue(event.target.value)} />
        </label>
        <label>帧数上限（可选） <input inputMode="numeric" maxLength={10}
          value={maximum} onChange={event => setMaximum(event.target.value)} /></label>
        <label><input type="checkbox" checked={includeKeyframes}
          onChange={event => setIncludeKeyframes(event.target.checked)} /> 包含人工关键帧</label>
        {trackRef && <label><input type="checkbox" checked={includeTracks}
          onChange={event => setSelectedTracks(event.target.checked ? trackRef.artifact_id : null)} />
          随样本保留当前追踪候选
        </label>}
        <button type="submit" disabled={!segment}>采样并登记待接收区</button>
      </fieldset>
    </form>
    {taskId && !error && <p role="status">正在读取实际视频帧并登记样本…</p>}
    {taskId && error && <button type="button" onClick={() => { setError(null); setRetry(value => value + 1); }}>重试查询</button>}
    {receipt && <p role="status">采样 {receipt.samples.length} 帧，其中待接收 {
      receipt.samples.filter(sample => !sample.already_in_dataset).length
    } 帧，已在数据集中 {receipt.samples.filter(sample => sample.already_in_dataset).length} 帧。</p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
