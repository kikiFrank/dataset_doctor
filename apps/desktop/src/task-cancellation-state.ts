import type { CancellationReceipt } from "./api/generated/wire-primitives";

export function cancellationState(status: string, observedStatus?: string): "AVAILABLE" | "REQUESTED" | "STOPPED" | "SETTLED" {
  if (["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"].includes(status)) {
    const latest = observedStatus ?? status;
    if (latest === "CANCELLED") return "STOPPED";
    if (latest === "CANCELLING" || status === "CANCELLING") return "REQUESTED";
    return ["QUEUED", "DISPATCHING", "RUNNING"].includes(latest) ? "AVAILABLE" : "SETTLED";
  }
  return status === "CANCELLED" ? "STOPPED" : "SETTLED";
}

export function cancellationReceiptMessage(receipt: Pick<CancellationReceipt, "requested" | "observed_status">): string {
  if (receipt.requested && receipt.observed_status === "CANCELLED") return "任务已停止。";
  if (receipt.requested) return "已请求取消，正在等待执行器确认停止。";
  return `任务已进入 ${receipt.observed_status}，保留当前结果。`;
}
