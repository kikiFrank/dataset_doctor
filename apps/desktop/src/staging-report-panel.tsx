import { useEffect, useState } from "react";
import type { StagingItem, StagingReportView } from "./api/generated/wire-primitives";
import { getStagingReport } from "./api/staging";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { matchesCurrentReport, reportCheckLabel } from "./staging-report-view";

const checkNames: Record<string, string> = {
  annotation_geometry: "标注几何",
  semantic_review: "语义检查",
};

const reasonMessages: Record<string, string> = {
  NO_ACCEPTED_EVIDENCE: "此样本没有已核验的语义检查结果。",
  ANNOTATION_MISSING: "此样本没有可用于几何检查的标注。",
};

export function StagingReportPanel({ workspace, item, onClose }: {
  workspace: OpenWorkspace;
  item: StagingItem;
  onClose: () => void;
}) {
  const [report, setReport] = useState<StagingReportView | null>(null);
  const [error, setError] = useState<string | null>(null);

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

  return <section aria-label={item.display_name + " 的检查报告"}>
    <h5>{item.display_name} 的检查报告</h5>
    <button type="button" onClick={onClose}>关闭报告</button>
    {!report && !error && <p role="status">正在核验检查报告…</p>}
    {error && <p role="alert">{error}</p>}
    {report && <ul>
      {report.checks.map(check => <li key={check.check_id}>
        <strong>{checkNames[check.check_id] ?? check.check_id}</strong> · {reportCheckLabel(check)}
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
  </section>;
}
