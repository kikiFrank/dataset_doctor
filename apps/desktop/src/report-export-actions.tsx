import { useEffect, useRef, useState } from "react";
import { selectExportDirectory } from "./api/dataset-export";
import type { ArtifactReference, ReportExportFormat } from "./api/generated/wire-primitives";
import { exportReport } from "./api/report-export";
import { getTask } from "./api/task";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";

const exportErrors: Record<string, string> = {
  REPORT_EVIDENCE_CHANGED: "报告内容已变化，请重新打开报告。",
  REPORT_OUTPUT_UNAVAILABLE: "无法写入所选目录。",
  REPORT_OUTPUT_UNVERIFIED: "无法核验导出文件，请检查所选目录。",
  SESSION_ENDED: "导出会话已结束，请检查所选目录。",
};

interface PendingExport {
  id: string;
  format: ReportExportFormat;
  directory: string;
}

export function ReportExportActions({ workspace, reference }: {
  workspace: OpenWorkspace;
  reference: ArtifactReference;
}) {
  const selecting = useRef(false);
  const [choosing, setChoosing] = useState(false);
  const [pending, setPending] = useState<PendingExport | null>(null);
  const [receipt, setReceipt] = useState<(PendingExport & { filename: string }) | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [queryRetry, setQueryRetry] = useState(0);

  useEffect(() => {
    if (!pending) return;
    const activeExport = pending;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const task = await getTask(workspace, activeExport.id);
        if (!active) return;
        if (task.status === "QUEUED" || task.status === "RUNNING") {
          timer = setTimeout(() => void poll(), 300);
          return;
        }
        setPending(null);
        const result = task.report_export;
        if (task.status === "SUCCEEDED" && result?.format === activeExport.format
            && result.report_ref.artifact_id === reference.artifact_id
            && result.report_ref.sha256 === reference.sha256) {
          setReceipt({ ...activeExport, filename: result.file_name });
        } else {
          setError(exportErrors[task.error_code ?? ""] ?? "无法核验报告导出的回执。");
        }
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [workspace, pending, reference.artifact_id, reference.sha256, queryRetry]);

  async function submit(format: ReportExportFormat) {
    if (selecting.current || pending) return;
    selecting.current = true;
    setChoosing(true);
    setError(null);
    try {
      const grant = await selectExportDirectory(workspace);
      if (!grant) return;
      const task = await exportReport(workspace, reference, format, grant.grant_id);
      setReceipt(null);
      setPending({ id: task.task_id, format, directory: grant.display_path });
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      selecting.current = false;
      setChoosing(false);
    }
  }

  const busy = choosing || pending !== null;
  return <div>
    <div className="review-actions">
      <button type="button" data-report-export="JSON" disabled={busy}
        onClick={() => void submit("JSON")}>导出 JSON</button>
      <button type="button" data-report-export="MARKDOWN" disabled={busy}
        onClick={() => void submit("MARKDOWN")}>导出 Markdown</button>
      {pending && error && <button type="button" onClick={() => {
        setError(null);
        setQueryRetry(value => value + 1);
      }}>重试查询</button>}
    </div>
    {pending && !error && <p role="status">正在导出报告…</p>}
    {error && <p role="alert">{error}</p>}
    {receipt && <div role="status" data-report-export-receipt={receipt.format}>
      <p>已导出 {receipt.filename}</p>
      <p className="import-path">{receipt.directory}</p>
    </div>}
  </div>;
}
