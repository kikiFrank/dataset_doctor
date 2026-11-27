import { useEffect, useRef, useState } from "react";
import type { StagingBatchReportView, StagingItem, StagingReportView } from "./api/generated/wire-primitives";
import { commitStaging, getStagingReport, prepareStagingBatch } from "./api/staging";
import { getTask } from "./api/task";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { findingsToAcknowledge, matchesCurrentReport, stagingCheckNames } from "./staging-report-view";

function sameAssets(actual: readonly string[], expected: readonly string[]): boolean {
  return actual.length === expected.length && [...actual].sort().every((id, index) => id === expected[index]);
}

export function StagingBatchReportPanel({ workspace, datasetId, items, acceptanceBlocked, onClose, onAccepted }: {
  workspace: OpenWorkspace;
  datasetId: string;
  items: readonly StagingItem[];
  acceptanceBlocked: boolean;
  onClose: () => void;
  onAccepted: () => void;
}) {
  const [verified, setVerified] = useState<{ batch: StagingBatchReportView; reports: StagingReportView[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reasons, setReasons] = useState<Record<string, string>>({});
  const [reviewed, setReviewed] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const attemptKey = useRef<string | null>(null);

  useEffect(() => {
    let current = true;
    async function load() {
      try {
        const refs = items.map(item => {
          const reference = item.current_report_ref;
          if (item.state !== "READY_FOR_ACCEPTANCE" || item.target_dataset_id !== datasetId || reference?.owner.kind !== "TASK") {
            throw new Error("选中的样本已变化，请刷新待接收区。");
          }
          return reference;
        });
        const reports: StagingReportView[] = [];
        for (const [index, ref] of refs.entries()) {
          const report = await getStagingReport(workspace, ref.owner.id);
          if (!current) return;
          if (!matchesCurrentReport(items[index], report)) throw new Error("检查报告已变化，请刷新待接收区。");
          reports.push(report);
        }
        const batch = await prepareStagingBatch(workspace, datasetId, refs, crypto.randomUUID());
        if (!current) return;
        if (batch.target_dataset_id !== datasetId || !sameAssets(batch.asset_ids, items.map(item => item.asset_id).sort())
            || reports.some(report => report.scope.label_schema_digest !== batch.label_schema_digest)) {
          throw new Error("批次报告与选中的样本不符，请刷新待接收区。");
        }
        setVerified({ batch, reports });
      } catch (failure) {
        if (current) setError(workspaceCommandError(failure));
      }
    }
    void load();
    return () => { current = false; };
  }, [workspace, datasetId, items]);

  const findings = new Map<string, { name: string; code: string; explanation: string }>();
  verified?.reports.forEach((report, index) => {
    for (const finding of findingsToAcknowledge(report)) {
      findings.set(finding.issue_id, {
        name: items[index].display_name, code: finding.finding_code, explanation: finding.explanation,
      });
    }
  });
  const gaps = verified?.batch.unexecuted_checks.map(check_id => ({
    check_id, reason: reasons[check_id]?.trim() ?? "",
  })) ?? [];
  const ready = verified && [...findings.keys()].every(id => reviewed.has(id)) && gaps.every(gap => gap.reason);

  async function accept() {
    if (!verified || !ready || busy || acceptanceBlocked) return;
    const { batch } = verified;
    attemptKey.current ??= crypto.randomUUID();
    setBusy(true);
    setError(null);
    try {
      const operation = await commitStaging(workspace, {
        target_dataset_id: datasetId, asset_ids: batch.asset_ids, quality_report_ref: batch.quality_report_ref,
        expected_schema_fingerprint: batch.label_schema_digest,
        acknowledged_findings: [...findings.keys()].map(issue_id => ({ issue_id })),
        acknowledged_incomplete_checks: gaps,
      }, attemptKey.current);
      const task = await getTask(workspace, operation.task_id);
      const receipt = task.staging_commit;
      if (task.status !== "SUCCEEDED" || receipt?.target_dataset_id !== datasetId
          || !sameAssets(receipt.accepted_asset_ids, [...batch.asset_ids].sort())
          || receipt.quality_report_ref.artifact_id !== batch.quality_report_ref.artifact_id
          || receipt.quality_report_ref.sha256 !== batch.quality_report_ref.sha256) {
        setError("无法核验批次接收回执，请刷新待接收区。");
        return;
      }
      onAccepted();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return <section aria-label="批次检查报告">
    <h5>{items.length} 个样本的接收确认</h5>
    <button type="button" disabled={busy} onClick={onClose}>关闭批次报告</button>
    {!verified && !error && <p role="status">正在核验选中的检查报告…</p>}
    {error && <p role="alert">{error}</p>}
    {verified && <fieldset disabled={busy || acceptanceBlocked}>
      <legend>接收到当前数据集</legend>
      {[...findings].map(([id, finding]) => <label key={id}>
        <input type="checkbox" checked={reviewed.has(id)} onChange={event => {
          const next = new Set(reviewed);
          if (event.target.checked) next.add(id);
          else next.delete(id);
          attemptKey.current = null;
          setReviewed(next);
        }} /> 已确认 {finding.name} · {finding.code} · {finding.explanation}
      </label>)}
      {gaps.map(({ check_id }) => <label key={check_id}>
        接收理由（{stagingCheckNames[check_id] ?? check_id}未执行）
        <textarea data-staging-batch-reason={check_id} required value={reasons[check_id] ?? ""} onChange={event => {
          attemptKey.current = null;
          setReasons({ ...reasons, [check_id]: event.target.value });
        }} />
      </label>)}
      <button type="button" data-staging-batch-commit disabled={!ready || busy} onClick={() => void accept()}>
        确认批次接收
      </button>
    </fieldset>}
    {busy && <p role="status">正在核验并接收批次…</p>}
  </section>;
}
