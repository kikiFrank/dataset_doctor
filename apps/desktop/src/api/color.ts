import { invoke } from "@tauri-apps/api/core";
import type { ColorApplySubmitRequest, ColorPreviewSubmitRequest, ImportOperationRef } from "./generated/wire-primitives";
import { context, type OpenWorkspace } from "./workspace";

export function previewColor(
  workspace: OpenWorkspace, intent: Omit<ColorPreviewSubmitRequest, "context">, key: string,
): Promise<ImportOperationRef> {
  const request: ColorPreviewSubmitRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: key }, ...intent,
  };
  return invoke("repair_color_preview", { request });
}

export function applyColor(
  workspace: OpenWorkspace, approval: Omit<ColorApplySubmitRequest, "context">, key: string,
): Promise<ImportOperationRef> {
  const request: ColorApplySubmitRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: key }, ...approval,
  };
  return invoke("repair_color_apply", { request });
}
