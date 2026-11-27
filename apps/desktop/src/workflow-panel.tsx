import { useEffect, useRef, useState } from "react";
import { TaskCancelButton } from "./task-cancel-button";
import type { WorkflowControlAction, WorkflowReportView, WorkflowStatusView, WorkflowView } from "./api/generated/wire-primitives";
import { completeWorkflowReview, controlWorkflow, getWorkflow, getWorkflowReport, listWorkflows, startWorkflow } from "./api/workflow";
import { workflowNeedsPolling } from "./workflow-review-state";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { ReportExportActions } from "./report-export-actions";
import { reportCheckLabel } from "./staging-report-view";

const statusNames: Record<WorkflowStatusView, string> = {
  DRAFT: "准备体检", READY: "计划就绪", RUNNING: "正在检查",
  PAUSED: "体检已暂停",
  COMPLETED: "体检已完成",
  INTERRUPTED: "体检已中断", WAITING_REVIEW: "等待人工审核", FAILED: "体检失败",
};
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
  INPUT_CHANGED: "复检期间输入已变化，请重新审核相关样本。",
  REVIEW_RECHECK_FAILED: "复检或 Final 核验未完成，请检查样本和任务状态后重新确认。",
  REVIEW_RECHECK_INTERRUPTED: "上次复检已中断，请重新确认后开始新的复检。",
};
const recheckStatusNames: Record<string, string> = {
  QUEUED: "等待复检", DISPATCHING: "正在派发", RUNNING: "正在复检",
  SUCCEEDED: "几何检查已完成", FAILED: "复检失败", INTERRUPTED: "复检已中断",
  CANCELLING: "正在停止", CANCELLED: "复检已取消",
};

function mergeWorkflow(records: WorkflowView[] | null, update: WorkflowView): WorkflowView[] {
  return records?.map(item => item.workflow_run_id === update.workflow_run_id
    && BigInt(item.row_revision) <= BigInt(update.row_revision) ? update : item) ?? [];
}

export function WorkflowPanel({ workspace, datasetId }: { workspace: OpenWorkspace; datasetId: string }) {
  const [records, setRecords] = useState<WorkflowView[] | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [report, setReport] = useState<WorkflowReportView | null>(null);
  const [limit, setLimit] = useState("100");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [pauseRequestedRun, setPauseRequestedRun] = useState<string | null>(null);
  const key = useRef<string | null>(null);
  const reviewKey = useRef<string | null>(null);
  const [reviewConfirmed, setReviewConfirmed] = useState(false);
  const [reviewReasons, setReviewReasons] = useState<Record<string, string>>({});
  const current = records?.find(item => item.workflow_run_id === selected);
  const status = current?.status;
  const loadingReport = current?.report_task_id ?? null;
  const running = current !== undefined && workflowNeedsPolling(current);
  const waitingForPause = status === "RUNNING" && pauseRequestedRun === selected;

  useEffect(() => {
    setReviewConfirmed(false);
    setReviewReasons({});
    reviewKey.current = null;
  }, [selected, loadingReport]);

  useEffect(() => {
    if (current?.error_code) {
      setReviewConfirmed(false);
      reviewKey.current = null;
    }
  }, [current?.error_code]);

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
        setRecords(previous => mergeWorkflow(previous, update));
        if (workflowNeedsPolling(update)) timer = setTimeout(() => void poll(), 300);
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

  async function completeReview() {
    if (busy || running || !current || !report) return;
    setBusy(true);
    setError(null);
    reviewKey.current ??= crypto.randomUUID();
    try {
      const update = await completeWorkflowReview(workspace, current, report, reviewReasons, reviewConfirmed, reviewKey.current);
      reviewKey.current = null;
      setReviewConfirmed(false);
      setRecords(previous => mergeWorkflow(previous, update));
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

  async function control(action: WorkflowControlAction) {
    if (busy || !current) return;
    setBusy(true);
    setError(null);
    try {
      const receipt = await controlWorkflow(workspace, current, action);
      if (receipt.workflow_run_id !== current.workflow_run_id || receipt.requested !== action) {
        throw new Error("无法核验体检控制回执。");
      }
      setPauseRequestedRun(action === "PAUSE" && receipt.blocking_task_ids.length > 0 ? receipt.workflow_run_id : null);
      const update = await getWorkflow(workspace, receipt.workflow_run_id);
      if (BigInt(update.row_revision) < BigInt(receipt.revision)) throw new Error("无法读取已更新的体检状态。");
      setRecords(previous => mergeWorkflow(previous, update));
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
    {records && records.length > 1 && <label>体检记录 <select value={selected ?? ""} disabled={busy}
      onChange={event => setSelected(event.target.value)}>
      {records.map(item => <option key={item.workflow_run_id} value={item.workflow_run_id}>
        {statusNames[item.status]} · {item.workflow_run_id.slice(0, 8)}
      </option>)}
    </select></label>}
    {cursor && <button type="button" disabled={busy} onClick={() => void more()}>更多体检记录</button>}
    {status && <p role="status">{statusNames[status]}</p>}
    {status === "RUNNING" && <button type="button" disabled={busy || waitingForPause} onClick={() => void control("PAUSE")}>
      暂停体检
    </button>}
    {status === "PAUSED" && <button type="button" disabled={busy} onClick={() => void control("RESUME")}>
      继续体检
    </button>}
    {waitingForPause && <p role="status">等待当前计算结束后暂停。</p>}
    {current?.error_code && <p role="alert">{runErrors[current.error_code] ?? current.error_code}</p>}
    {current && current.review_recheck_tasks.length > 0 && <section aria-label="审核后复检">
      <h5>审核后复检</h5>
      {current.review_recheck_pending && <p role="status">正在核验 Final 并运行复检…</p>}
      <ul>{current.review_recheck_tasks.map(task => <li key={task.task_id}>
        样本 {task.asset_id.slice(0, 8)} · {recheckStatusNames[task.status] ?? task.status}
        <TaskCancelButton workspace={workspace} taskId={task.task_id} status={task.status} />
      </li>)}</ul>
    </section>}
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
      {report.human_ack_status === "ACKNOWLEDGED" && <p role="status">
        审核已确认，{report.repair_outcomes.length} 个样本的 Final 与复检证据已核验。
      </p>}
      <ReportExportActions key={report.report_id} workspace={workspace} reference={report.report_artifact_ref} />
      {status === "WAITING_REVIEW" && <form aria-label="确认审核并复检"
        onSubmit={event => { event.preventDefault(); void completeReview(); }}>
        <p>请先完成报告涉及样本的 Final 审核，再确认未执行检查的范围。</p>
        {report.unexecuted_checks.map(check => <label key={check}>
          {checkNames[check] ?? check}的未执行确认理由
          <textarea required value={reviewReasons[check] ?? ""} disabled={busy || running}
            onChange={event => {
              const value = event.currentTarget.value;
              reviewKey.current = null;
              setReviewConfirmed(false);
              setReviewReasons(previous => ({ ...previous, [check]: value }));
            }} />
        </label>)}
        <label><input type="checkbox" checked={reviewConfirmed} disabled={busy || running}
          onChange={event => setReviewConfirmed(event.currentTarget.checked)} />
          已完成相关样本的 Final 审核，并确认上述未执行检查及理由
        </label>
        <button type="submit" disabled={busy || running || !reviewConfirmed}>确认审核并复检</button>
        {current?.review_recheck_pending && <p role="status">正在等待复检与 Core 核验完成…</p>}
      </form>}
      <h6>{report.human_ack_status === "ACKNOWLEDGED" ? "初次检查结果" : "检查结果"}</h6>
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
