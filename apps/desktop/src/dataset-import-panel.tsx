import { useEffect, useRef, useState } from "react";
import { confirmImport, getImportTask, inspectImport, pickImportDirectory } from "./api/dataset-import";
import type { DatasetImportPreview, ImportOperationRef, PathGrant } from "./api/generated/wire-primitives";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const blockedReasons: Record<string, string> = {
  IMAGE_UNREADABLE: "无法读取图片",
  ANNOTATION_UNREADABLE: "无法读取标注",
  AMBIGUOUS_SIDECAR: "同名图片的标注归属不明确",
  WRONG_IMAGE: "标注引用了其他图片",
  ANNOTATION_TOO_LARGE: "标注超过大小限制",
  UNSUPPORTED_ANNOTATION: "标注不符合单图 COCO 框格式",
};

const taskErrors: Record<string, string> = {
  DIRECTORY_GRANT_INVALID: "目录授权已失效，请重新选择。",
  IMPORT_PREVIEW_FAILED: "无法预检此目录。请检查目录权限、图片数量和文件格式。",
  INPUT_CHANGED: "源文件已发生变化，请重新选择目录并预检。",
  SESSION_ENDED: "工作区会话已结束，请重新预检。",
  APPROVAL_UNAVAILABLE: "预览已失效，请重新预检。",
};

export function DatasetImportPanel(
  { workspace, onImported }: { workspace: OpenWorkspace; onImported: () => void },
) {
  const [name, setName] = useState("");
  const [selection, setSelection] = useState<PathGrant | null>(null);
  const [preview, setPreview] = useState<DatasetImportPreview | null>(null);
  const [operation, setOperation] = useState<ImportOperationRef | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [queryRetry, setQueryRetry] = useState(0);
  const inspectKey = useRef(crypto.randomUUID());
  const confirmKey = useRef(crypto.randomUUID());

  useEffect(() => {
    if (!operation) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const taskId = operation.task_id;
    async function poll() {
      try {
        const task = await getImportTask(workspace, taskId);
        if (!active) return;
        if (task.status === "QUEUED" || task.status === "RUNNING") {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setOperation(null);
        setMessage(null);
        if (task.status === "SUCCEEDED" && task.preview) {
          setPreview(task.preview);
        } else if (task.status === "SUCCEEDED" && task.registration) {
          setSelection(null);
          setPreview(null);
          setName("");
          setMessage("已登记 " + task.registration.asset_count + " 张图片。");
          onImported();
        } else {
          setError(taskErrors[task.error_code ?? ""] ?? "导入未完成，请检查输入后重新预检。");
        }
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [operation, workspace, onImported, queryRetry]);

  async function choose() {
    setError(null);
    setMessage(null);
    setBusy(true);
    try {
      const chosen = await pickImportDirectory(workspace);
      if (!chosen) return;
      inspectKey.current = crypto.randomUUID();
      confirmKey.current = crypto.randomUUID();
      setSelection(chosen);
      setPreview(null);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function submit(confirm: boolean) {
    if (!selection || (confirm && !preview)) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const started = confirm && preview
        ? await confirmImport(workspace, preview, confirmKey.current)
        : await inspectImport(workspace, selection.grant_id, name.trim(), inspectKey.current);
      setOperation(started);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  const disabled = busy || operation !== null;
  return (
    <form className="dataset-import" onSubmit={(event) => {
      event.preventDefault();
      void submit(preview !== null);
    }}>
      <h3>导入图片与标注</h3>
      <label htmlFor="import-name">新数据集名称</label>
      <input id="import-name" required maxLength={128} value={name} disabled={disabled} onChange={(event) => {
        setName(event.target.value);
        if (preview) setSelection(null);
        setPreview(null);
        inspectKey.current = crypto.randomUUID();
        confirmKey.current = crypto.randomUUID();
      }} />
      <button type="button" disabled={disabled} onClick={() => void choose()}>选择目录</button>
      {selection && <p className="import-path">{selection.display_path}</p>}
      {operation && !error && <p role="status">
        {operation.operation_kind === "DATASET_IMPORT_INSPECT" ? "正在预检目录…" : "正在登记数据集…"}
      </p>}
      {error && <p role="alert">{error}</p>}
      {operation && error && <button type="button" onClick={() => {
        setError(null);
        setQueryRetry((value) => value + 1);
      }}>重新查询任务</button>}
      {message && <p role="status">{message}</p>}
      {preview && (
        <>
          <p>{preview.files.length} 张图片。确认后会登记原目录中的文件。</p>
          {preview.category_names.length > 0 && <p>类别：{preview.category_names.join("、")}</p>}
          <ul className="import-preview">
            {preview.files.map((file) => (
              <li key={file.image_path}>
                <span>{file.image_path}</span>
                <small>{file.annotation_state === "BLOCKED"
                  ? blockedReasons[file.issue_code ?? ""] ?? "文件不可导入"
                  : file.annotation_path ?? "没有标注文件"}</small>
              </li>
            ))}
          </ul>
        </>
      )}
      <button type="submit" disabled={disabled || !selection || !name.trim() || (preview !== null && !preview.can_confirm)}>
        {preview ? "确认导入" : "预检"}
      </button>
    </form>
  );
}
