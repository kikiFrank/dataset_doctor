import { invoke } from "@tauri-apps/api/core";
import type { CancellationReceipt, TaskCancelRequest, GeometrySubmitRequest, ImportOperationRef, ImportTaskView, TaskGetRequest } from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function getTask(workspace: OpenWorkspace, taskId: string): Promise<ImportTaskView> {
  const request: TaskGetRequest = { context: context(workspace, crypto.randomUUID()), task_id: taskId };
  return invoke("task_get", { request });
}

export function cancelTask(workspace: OpenWorkspace, taskId: string): Promise<CancellationReceipt> {
  const request: TaskCancelRequest = { context: context(workspace, crypto.randomUUID()), task_id: taskId };
  return invoke("task_cancel", { request });
}

export function startGeometry(
  workspace: OpenWorkspace, datasetId: string, assetId: string, idempotencyKey: string,
): Promise<ImportOperationRef> {
  const request: GeometrySubmitRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    dataset_id: datasetId,
    asset_id: assetId,
  };
  return invoke("annotation_geometry_submit", { request });
}
