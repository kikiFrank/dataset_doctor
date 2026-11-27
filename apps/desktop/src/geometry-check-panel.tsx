import { useEffect, useRef, useState } from "react";
import { TaskCancelButton } from "./task-cancel-button";
import type { AssetSummary, GeometryReportView } from "./api/generated/wire-primitives";
import { getTask, startGeometry } from "./api/task";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const activeStates = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);
const errorMessages: Record<string, string> = {
  WORKER_START_FAILED: "检查进程无法启动，请检查本机 Worker 配置。",
  WORKER_INTERRUPTED: "检查已中断，可以重新运行。",
};

export type GeometryEvidence = { taskId: string; report: GeometryReportView };

export function GeometryCheckPanel({ workspace, asset, onEvidence, disabled = false }: {
  workspace: OpenWorkspace;
  asset: AssetSummary;
  onEvidence: (evidence: GeometryEvidence | null) => void;
  disabled?: boolean;
}) {
  const [taskId, setTaskId] = useState<string | null>(null);
  const [taskStatus, setTaskStatus] = useState("QUEUED");
  const [report, setReport] = useState<GeometryReportView | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [retryQuery, setRetryQuery] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const key = useRef<string | null>(null);

  useEffect(() => {
    if (!taskId) return;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const id = taskId;
    async function poll() {
      try {
        const task = await getTask(workspace, id);
        if (!current) return;
        setTaskStatus(task.status);
        if (activeStates.has(task.status)) {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setTaskId(null);
        key.current = null;
        if (task.status === "SUCCEEDED" && task.geometry_report) {
          setReport(task.geometry_report);
          onEvidence({ taskId: id, report: task.geometry_report });
        } else {
          setError(task.status === "CANCELLED" ? "检查已取消，可以重新运行。" : errorMessages[task.error_code ?? ""] ?? "检查未完成，可以重新运行。");
        }
      } catch (failure) {
        if (current) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { current = false; clearTimeout(timer); };
  }, [workspace, taskId, retryQuery, onEvidence]);

  async function submit() {
    if (disabled || submitting || taskId) return;
    key.current ??= crypto.randomUUID();
    setSubmitting(true);
    setError(null);
    setReport(null);
    onEvidence(null);
    try {
      if (!asset.dataset_id) throw new Error("请选择数据集中的图片。");
      const operation = await startGeometry(workspace, asset.dataset_id, asset.asset_id, key.current);
      setTaskId(operation.task_id);
      setTaskStatus(operation.status);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <section aria-label="标注几何检查">
      <button type="button" disabled={disabled || submitting || taskId !== null || asset.availability === "OFFLINE"} onClick={() => void submit()}>
        运行标注检查
      </button>
      {(submitting || taskId) && !error && <p role="status">正在检查图像和标注…</p>}
      {taskId && <TaskCancelButton key={taskId} workspace={workspace} taskId={taskId} status={taskStatus} />}
      {error && <p role="alert">{error}</p>}
      {error && taskId && <button type="button" onClick={() => { setError(null); setRetryQuery((value) => value + 1); }}>重试查询</button>}
      {report && (
        <>
          <p>实际图像尺寸 {report.width} × {report.height} 像素</p>
          {report.status === "NOT_EXECUTED" && <p>缺少标注文件，已读取图像尺寸。</p>}
          {report.status === "COMPLETED" && report.findings.length === 0 && <p>几何检查通过。</p>}
          {report.findings.length > 0 && <ul>
            {report.findings.map((finding, index) => (
              <li key={index}>{finding.code}{finding.annotation_id === null ? "" : ` · 标注 ${finding.annotation_id}`}</li>
            ))}
          </ul>}
        </>
      )}
    </section>
  );
}
