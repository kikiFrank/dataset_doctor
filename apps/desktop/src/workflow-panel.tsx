import { useEffect, useRef, useState } from "react";
import type { WorkflowReportView, WorkflowStatusView, WorkflowView } from "./api/generated/wire-primitives";
import { getWorkflow, getWorkflowReport, listWorkflows, startWorkflow } from "./api/workflow";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { ReportExportActions } from "./report-export-actions";
import { reportCheckLabel } from "./staging-report-view";

const statusNames: Record<WorkflowStatusView, string> = {
  DRAFT: "准备体检", READY: "计划就绪", RUNNING: "正在检查",
  PAUSED: "体检已暂停",
  INTERRUPTED: "体检已中断", WAITING_REVIEW: "等待人工审核", FAILED: "体检失败",
};
const activeStatuses = new Set<WorkflowStatusView>(["DRAFT", "READY", "RUNNING"]);
const checkNames: Record<string, string> = {
  dataset_profile: "数据画像", annotation_geometry: "标注几何",
  image_quality: "图像质量", detector_predictions: "检测预测", semantic_review: "语义检查",
};
const reasonNames: Record<string, string> = {
  MODEL_NOT_CONFIGURED: "未配置模型。",
  CAPABILITY_UNAVAILABLE: "所需能力未就绪。",
  INCOMPLETE_COVERAGE: "部分样本未获得可评估结果。",
};
const runErrors: Record<string, string> = {
  WORKER_START_FAILED: "本机 Worker 启动失败。",
  WORKER_INTERRUPTED: "本机 Worker 运行已中断。",
  SESSION_ENDED: "此前工作区会话已结束。",
};

export function WorkflowPanel({ workspace, datasetId }: { workspace: OpenWorkspace; datasetId: string }) {
  const [records, setRecords] = useState<WorkflowView[] | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [report, setReport] = useState<WorkflowReportView | null>(null);
  const [limit, setLimit] = useState("100");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const key = useRef<string | null>(null);
  const current = records?.find(item => item.workflow_run_id === selected);
  const status = current?.status;
  const loadingReport = current?.report_task_id ?? null;
  const running = status !== undefined && activeStatuses.has(status);

  useEffect(() => {
    let active = true;
    void listWorkflows(workspace, datasetId).then(page => {
      if (!active) return;
      setRecords(page.items);
      setCursor(page.next_cursor);
      setSelected(previous => page.items.find(item => item.workflow_run_id === previous)?.workflow_run_id
        ?? page.items[0]?.workflow_run_id ?? null);
    }).catch(failure => { if (active) setError(workspaceCommandError(failure)); });
    return () => { active = false; };
  }, [workspace, datasetId, refresh]);

  useEffect(() => {
    if (!selected || !running) return;
    const runId = selected;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const update = await getWorkflow(workspace, runId);
        if (!active) return;
        setRecords(previous => previous?.map(item => item.workflow_run_id === runId ? update : item) ?? []);
        if (activeStatuses.has(update.status)) timer = setTimeout(() => void poll(), 300);
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [workspace, selected, running, refresh]);

  useEffect(() => {
    setReport(null);
    if (!selected || !loadingReport) return;
    let active = true;
    void getWorkflowReport(workspace, selected).then(data => {
      if (!active) return;
      if (data.scope.dataset_id !== datasetId || data.scope.workflow_run_id !== selected) {
        setError("无法核验报告归属。");
        return;
      }
      setReport(data);
    }).catch(failure => { if (active) setError(workspaceCommandError(failure)); });
    return () => { active = false; };
  }, [workspace, datasetId, selected, loadingReport, refresh]);

  async function start() {
    const sampleLimit = Number(limit);
    if (busy || records === null || running) return;
    if (!Number.isInteger(sampleLimit) || sampleLimit < 1 || sampleLimit > 100_000) {
      setError("抽样上限须为 1 到 100000 的整数。");
      return;
    }
    setBusy(true);
    setError(null);
    key.current ??= crypto.randomUUID();
    try {
      const started = await startWorkflow(workspace, datasetId, sampleLimit, key.current);
      key.current = null;
      setRecords(previous => [started, ...(previous ?? []).filter(item => item.workflow_run_id !== started.workflow_run_id)]);
      setSelected(started.workflow_run_id);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function more() {
    if (!cursor || busy) return;
    setBusy(true);
    setError(null);
    try {
      const page = await listWorkflows(workspace, datasetId, cursor);
      setRecords(previous => [...(previous ?? []), ...page.items]);
      setCursor(page.next_cursor);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return <section className="workflow-panel" aria-label="数据体检">
    <h4>数据体检</h4>
    <form onSubmit={event => { event.preventDefault(); void start(); }}>
      <label>抽样上限 <input type="number" min={1} max={100000} value={limit} required
        disabled={busy || running} onChange={event => { key.current = null; setLimit(event.target.value); }} /></label>
      <button type="submit" disabled={busy || running || records === null}>开始本地体检</button>
    </form>
    {records === null && !error && <p role="status">正在读取体检记录…</p>}
    {records && records.length > 1 && <label>体检记录 <select value={selected ?? ""}
      onChange={event => setSelected(event.target.value)}>
      {records.map(item => <option key={item.workflow_run_id} value={item.workflow_run_id}>
        {statusNames[item.status]} · {item.workflow_run_id.slice(0, 8)}
      </option>)}
    </select></label>}
    {cursor && <button type="button" disabled={busy} onClick={() => void more()}>更多体检记录</button>}
    {status && <p role="status">{statusNames[status]}</p>}
    {current?.error_code && <p>{runErrors[current.error_code] ?? current.error_code}</p>}
    {running && current && current.geometry_tasks.length > 0 && <p>
      标注几何 {current.geometry_tasks.filter(task => task.status === "SUCCEEDED").length} / {current.geometry_tasks.length}
    </p>}
    {error && <>
      <p role="alert">{error}</p>
      <button type="button" onClick={() => { setError(null); setRefresh(value => value + 1); }}>刷新体检记录</button>
    </>}
    {loadingReport && !report && !error && <p role="status">正在核验质量报告…</p>}
    {report && report.scope.workflow_run_id === selected && <section aria-label="质量报告">
      <h5>质量报告</h5>
      <ReportExportActions key={report.report_id} workspace={workspace} reference={report.report_artifact_ref} />
      <ul>
        {report.checks.map(check => <li key={check.check_id}>
          <strong>{checkNames[check.check_id] ?? check.check_id}</strong> · {reportCheckLabel(check)}
          {" · "}{check.processed_assets} / {check.total_assets} 个样本
          {check.skip_or_failure_reason && <p>
            {reasonNames[check.skip_or_failure_reason.code] ?? check.skip_or_failure_reason.message}
          </p>}
          {check.findings.length > 0 && <ul>
            {check.findings.map(finding => <li key={finding.issue_id}>
              {finding.finding_code} · {finding.explanation}
            </li>)}
          </ul>}
        </li>)}
      </ul>
    </section>}
  </section>;
}
