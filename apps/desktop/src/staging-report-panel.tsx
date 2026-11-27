import { useEffect, useRef, useState } from "react";
import type { StagingItem, StagingReportView } from "./api/generated/wire-primitives";
import { commitStaging, getStagingReport } from "./api/staging";
import { getTask } from "./api/task";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { ReportExportActions } from "./report-export-actions";
import {
  findingsToAcknowledge, matchesCurrentReport, reportCheckLabel, stagingCheckNames, stagingCommitDetails,
} from "./staging-report-view";

const reasonMessages: Record<string, string> = {
  NO_ACCEPTED_EVIDENCE: "此样本没有已核验的语义检查结果。",
  ANNOTATION_MISSING: "此样本没有可用于几何检查的标注。",
};

export function StagingReportPanel({ workspace, item, acceptanceBlocked, onClose, onAccepted }: {
  workspace: OpenWorkspace;
  item: StagingItem;
  acceptanceBlocked: boolean;
  onClose: () => void;
  onAccepted: () => void;
}) {
  const [report, setReport] = useState<StagingReportView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reasons, setReasons] = useState<Record<string, string>>({});
  const [reviewedFindings, setReviewedFindings] = useState(new Set<string>());
  const [busy, setBusy] = useState(false);
  const attemptKey = useRef<string | null>(null);

  useEffect(() => {
    const owner = item.current_report_ref?.owner;
    if (owner?.kind !== "TASK") return;
    let current = true;
    setReport(null);
    setError(null);
    void getStagingReport(workspace, owner.id).then(loaded => {
      if (!current) return;
      if (matchesCurrentReport(item, loaded)) setReport(loaded);
      else setError("检查报告已变化，请刷新待接收区。");
    }).catch(failure => {
      if (current) setError(workspaceCommandError(failure));
    });
    return () => { current = false; };
  }, [workspace, item]);

  const details = report ? stagingCommitDetails(item, report, reasons, reviewedFindings) : null;

  async function accept() {
    if (!details || busy || acceptanceBlocked) return;
    attemptKey.current ??= crypto.randomUUID();
    setBusy(true);
    setError(null);
    try {
      const operation = await commitStaging(workspace, details, attemptKey.current);
      const task = await getTask(workspace, operation.task_id);
      const receipt = task.staging_commit;
      if (task.status !== "SUCCEEDED" || receipt?.target_dataset_id !== details.target_dataset_id
          || receipt.accepted_asset_ids.length !== 1 || receipt.accepted_asset_ids[0] !== item.asset_id
          || receipt.quality_report_ref.sha256 !== details.quality_report_ref.sha256) {
        setError("无法核验接收回执，请刷新待接收区。");
        return;
      }
      onAccepted();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return <section aria-label={item.display_name + " 的检查报告"}>
    <h5>{item.display_name} 的检查报告</h5>
    <button type="button" disabled={busy} onClick={onClose}>关闭报告</button>
    {!report && !error && <p role="status">正在核验检查报告…</p>}
    {error && <p role="alert">{error}</p>}
    {report && <ReportExportActions key={report.report_id} workspace={workspace} reference={report.report_artifact_ref} />}
    {report && <ul>
      {report.checks.map(check => <li key={check.check_id}>
        <strong>{stagingCheckNames[check.check_id] ?? check.check_id}</strong> · {reportCheckLabel(check)}
        {" · "}{check.processed_assets} / {check.total_assets} 个样本
        {check.skip_or_failure_reason && <p>
          {reasonMessages[check.skip_or_failure_reason.code] ?? check.skip_or_failure_reason.message}
        </p>}
        {check.findings.length > 0 && <ul>
          {check.findings.map(finding => <li key={finding.issue_id}>
            {finding.finding_code}{" · "}{finding.explanation}
          </li>)}
        </ul>}
      </li>)}
    </ul>}
    {report && item.state === "READY_FOR_ACCEPTANCE" && <fieldset disabled={busy || acceptanceBlocked}>
      <legend>接收到当前数据集</legend>
      {findingsToAcknowledge(report).map(finding => <label key={finding.issue_id}>
        <input type="checkbox" checked={reviewedFindings.has(finding.issue_id)} onChange={event => {
          const reviewed = new Set(reviewedFindings);
          if (event.target.checked) reviewed.add(finding.issue_id);
          else reviewed.delete(finding.issue_id);
          attemptKey.current = null;
          setReviewedFindings(reviewed);
        }} /> 已确认 {finding.finding_code}
      </label>)}
      {report.unexecuted_checks.map(check => <label key={check}>
        接收理由（{stagingCheckNames[check] ?? check}未执行）
        <textarea required value={reasons[check] ?? ""} onChange={event => {
          attemptKey.current = null;
          setReasons({ ...reasons, [check]: event.target.value });
        }} />
      </label>)}
      <button type="button" disabled={!details || busy} onClick={() => void accept()}>确认接收</button>
    </fieldset>}
    {busy && <p role="status">正在核验并接收样本…</p>}
  </section>;
}
