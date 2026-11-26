import type { StagingItem, StagingReportCheckView, StagingReportView } from "./api/generated/wire-primitives";

export function matchesCurrentReport(item: StagingItem, report: StagingReportView): boolean {
  const current = item.current_report_ref;
  const returned = report.report_artifact_ref;
  return current?.owner.kind === "TASK" && returned.owner.kind === "TASK"
    && current.owner.id === report.report_id && returned.owner.id === report.report_id
    && current.artifact_id === returned.artifact_id && current.sha256 === returned.sha256
    && current.kind === returned.kind && current.media_type === returned.media_type
    && current.size_bytes === returned.size_bytes && current.schema_id === returned.schema_id
    && report.scope.asset_id === item.asset_id && report.scope.dataset_id === item.target_dataset_id;
}

export function reportCheckLabel(check: Pick<StagingReportCheckView, "execution_status" | "assessment">): string {
  switch (check.execution_status) {
    case "SKIPPED": return "未执行 · 未评估";
    case "UNSUPPORTED": return "不支持 · 未评估";
    case "FAILED": return "执行失败 · 未评估";
    case "CANCELLED": return "已取消 · 未评估";
    case "COMPLETED":
      switch (check.assessment) {
        case "PASS": return "通过";
        case "FINDINGS": return "发现问题";
        case "INCONCLUSIVE": return "无法判定";
        default: return "未评估";
      }
    default: return "未评估";
  }
}
