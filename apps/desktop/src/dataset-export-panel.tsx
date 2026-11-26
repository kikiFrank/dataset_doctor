import { useEffect, useRef, useState } from "react";
import { exportDataset, selectExportDirectory } from "./api/dataset-export";
import type { CocoExportView, PathGrant } from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const errors: Record<string, string> = {
  EXPORT_BLOCKED: "存在未确认、已过期、离线或正在审核的样本。请处理这些样本，或明确排除后导出。",
  NO_ELIGIBLE_IMAGES: "没有符合导出条件的已确认样本。",
  INPUT_CHANGED: "导出期间样本或目录已变化。请重新检查后再导出。",
  SESSION_ENDED: "导出会话已结束，请检查目标目录。",
  EXPORT_RECOVERY_REQUIRED: "导出回执待恢复，请重新打开工作区并检查目标目录。",
  EXPORT_OUTPUT_UNVERIFIED: "无法核验目标目录中的数据包，请检查文件后重新导出。",
};

export function DatasetExportPanel({ workspace, datasetId }: { workspace: OpenWorkspace; datasetId: string }) {
  const [grant, setGrant] = useState<PathGrant | null>(null);
  const [exclude, setExclude] = useState(false);
  const [busy, setBusy] = useState(false);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<CocoExportView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [queryRetry, setQueryRetry] = useState(0);
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
        if (task.status === "QUEUED" || task.status === "RUNNING") {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setTaskId(null);
        setGrant(null);
        key.current = null;
        if (task.status === "SUCCEEDED" && task.coco_export) {
          setReceipt(task.coco_export);
        } else {
          setError(errors[task.error_code ?? ""] ?? "导出未完成，请检查目录和样本。");
        }
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [workspace, taskId, queryRetry]);

  async function select() {
    setBusy(true);
    setError(null);
    try {
      const selected = await selectExportDirectory(workspace);
      if (!selected) return;
      key.current = null;
      setGrant(selected);
      setReceipt(null);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function submit() {
    if (!grant || busy || taskId) return;
    key.current ??= crypto.randomUUID();
    setBusy(true);
    setError(null);
    setReceipt(null);
    try {
      const operation = await exportDataset(workspace, datasetId, grant.grant_id, exclude ? "EXCLUDE_INELIGIBLE" : "STRICT", key.current);
      setTaskId(operation.task_id);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  const disabled = busy || taskId !== null;
  return (
    <details className="dataset-export">
      <summary>导出 COCO 训练数据</summary>
      <form onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <p>导出当前数据集中已确认的正式标注。</p>
        <label><input type="checkbox" checked={exclude} disabled={disabled || key.current !== null} onChange={(event) => setExclude(event.target.checked)} />
          排除不符合条件的样本，并在清单中记录原因</label>
        <div className="review-actions">
          <button type="button" disabled={disabled} onClick={() => void select()}>选择导出目录</button>
          <button type="submit" disabled={disabled || !grant}>导出</button>
        </div>
        {grant && <p className="import-path">{grant.display_path}</p>}
        {taskId && !error && <p role="status">正在生成 COCO 数据包…</p>}
        {taskId && error && <button type="button" onClick={() => { setError(null); setQueryRetry((value) => value + 1); }}>重试查询</button>}
        {error && <p role="alert">{error}</p>}
        {receipt && <div role="status">
          <p>已导出 {receipt.image_count} 张图片和 {receipt.annotation_count} 个标注框，排除 {receipt.excluded_count} 张图片。</p>
          <p className="import-path">{receipt.package_path}</p>
        </div>}
      </form>
    </details>
  );
}
