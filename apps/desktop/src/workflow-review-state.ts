import type { WorkflowCompleteReviewRequest, WorkflowReportView, WorkflowView } from "./api/generated/wire-primitives";

export function workflowNeedsPolling(workflow: WorkflowView): boolean {
  if (["DRAFT", "READY", "RUNNING"].includes(workflow.status)) return true;
  return workflow.status === "WAITING_REVIEW" && workflow.review_recheck_pending
    && !workflow.error_code
    && !workflow.review_recheck_tasks.some(task => ["FAILED", "INTERRUPTED", "CANCELLED"].includes(task.status));
}

export function workflowCanResume(workflow: WorkflowView): boolean {
  if (workflow.status !== "PAUSED") return false;
  const profile = workflow.profile_task?.status;
  if (!workflow.geometry_tasks.length) return profile === "QUEUED" || profile === "SUCCEEDED";
  return profile === "SUCCEEDED"
    && workflow.geometry_tasks.every(task => task.status === "QUEUED" || task.status === "SUCCEEDED");
}

export function reviewCompletionPayload(
  workflow: WorkflowView, report: WorkflowReportView, reasons: Record<string, string>, confirmed: boolean,
): Omit<WorkflowCompleteReviewRequest, "context"> {
  if (workflow.status !== "WAITING_REVIEW" || report.human_ack_status !== "PENDING"
    || report.scope.workflow_run_id !== workflow.workflow_run_id) {
    throw new Error("请刷新并选择当前等待审核的体检报告。");
  }
  if (workflowNeedsPolling(workflow)) throw new Error("正在复检，请等待当前结果。");
  if (!confirmed) throw new Error("请明确确认已完成相关样本的 Final 审核。");
  const acknowledgments = report.unexecuted_checks.map(check => ({ check_id: check, reason: reasons[check]?.trim() ?? "" }));
  if (acknowledgments.some(entry => !entry.reason)) throw new Error("请为每项未执行检查填写确认理由。");
  return {
    workflow_run_id: workflow.workflow_run_id, expected_revision: workflow.row_revision,
    expected_report: report.report_artifact_ref, acknowledged_incomplete_checks: acknowledgments,
  };
}
