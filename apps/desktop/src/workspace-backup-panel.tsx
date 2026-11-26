import { useEffect, useRef, useState } from "react";

import type { ImportTaskView, PathGrant, WorkspaceBackupView } from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { latestBackup, selectBackupDirectory, startBackup } from "./api/workspace-backup";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";

const errors: Record<string, string> = {
  DESTINATION_EXISTS: "备份目录已存在，目录内容已保留。",
  DESTINATION_UNAVAILABLE: "所选目录无法访问，请重新选择。",
  BACKUP_INPUT_UNVERIFIED: "工作区中有登记文件缺失或已修改，请检查资源后重试。",
  WORKSPACE_RECOVERY_REQUIRED: "工作区有待对账的文件操作，请先恢复该操作再备份。",
  SESSION_ENDED: "上一次备份会话已结束，可以重新发起备份。",
  BACKUP_OUTPUT_UNVERIFIED: "无法核验上一次发布的备份，请检查目标目录。",
};

export function WorkspaceBackupPanel({ workspace }: { workspace: OpenWorkspace }) {
  const [grant, setGrant] = useState<PathGrant | null>(null);
  const [task, setTask] = useState<ImportTaskView | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [queryRetry, setQueryRetry] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const key = useRef<string | null>(null);

  useEffect(() => {
    let mounted = true;
    void latestBackup(workspace).then((latest) => {
      if (!mounted || !latest) return;
      if (latest.status === "QUEUED" || latest.status === "RUNNING") setPending(latest.task_id);
      else setTask(latest);
    }).catch((failure: unknown) => {
      if (mounted) setError(workspaceCommandError(failure));
    }).finally(() => { if (mounted) setBusy(false); });
    return () => { mounted = false; };
  }, [workspace]);

  useEffect(() => {
    if (pending === null) return;
    const taskId = pending;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const record = await getTask(workspace, taskId);
        if (!active) return;
        if (record.status === "QUEUED" || record.status === "RUNNING") {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setPending(null);
        setTask(record);
        setGrant(null);
        key.current = null;
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [workspace, pending, queryRetry]);

  async function act(operation: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await operation();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function choose() {
    await act(async () => {
      const selected = await selectBackupDirectory(workspace);
      if (!selected) return;
      setGrant(selected);
      setTask(null);
      key.current = null;
    });
  }

  async function submit() {
    if (!grant || busy || pending) return;
    const idempotencyKey = key.current ??= crypto.randomUUID();
    await act(async () => {
      const operation = await startBackup(workspace, grant.grant_id, idempotencyKey);
      setPending(operation.task_id);
      setTask(null);
    });
  }

  const receipt: WorkspaceBackupView | null = task?.workspace_backup ?? null;
  const taskFailure = task && task.status !== "SUCCEEDED"
    ? errors[task.error_code ?? ""] ?? "备份未完成，请检查工作区和目标目录。"
    : null;
  const disabled = busy || pending !== null;
  return (
    <details className="dataset-export">
      <summary>备份工作区</summary>
      <form onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <p>备份包含已登记的资源、标注和报告。</p>
        <div className="review-actions">
          <button type="button" disabled={disabled} onClick={() => void choose()}>选择备份位置</button>
          <button type="submit" disabled={disabled || !grant}>开始备份</button>
        </div>
        {grant && <p className="import-path">{grant.display_path}</p>}
        {pending && !error && <p role="status">正在生成工作区备份…</p>}
        {pending && error && <button type="button" onClick={() => { setError(null); setQueryRetry((value) => value + 1); }}>重试查询</button>}
        {(error || taskFailure) && <p role="alert">{error ?? taskFailure}</p>}
        {receipt && <div role="status">
          <p>备份已保存，包含 {receipt.asset_file_count} 个资源文件。</p>
          <p className="import-path">{receipt.package_path}</p>
        </div>}
      </form>
    </details>
  );
}
