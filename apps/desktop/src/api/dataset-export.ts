import { invoke } from "@tauri-apps/api/core";
import type { DatasetExportSubmitRequest, ExportPolicy, ImportOperationRef, PathGrant, PathSelectRequest } from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function selectExportDirectory(workspace: OpenWorkspace): Promise<PathGrant | null> {
  const requestId = crypto.randomUUID();
  const request: PathSelectRequest = {
    request_id: requestId, context: context(workspace, requestId),
    purpose: "EXPORT", kind: "DIRECTORY", access: "READ_WRITE",
  };
  return invoke("path_select", { request });
}

export function exportDataset(
  workspace: OpenWorkspace, datasetId: string, grantId: string, policy: ExportPolicy, key: string,
): Promise<ImportOperationRef> {
  const request: DatasetExportSubmitRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: key },
    dataset_id: datasetId,
    destination_grant_id: grantId,
    format: "COCO_DETECTION", selection: "ALL_MEMBERS", export_policy: policy,
  };
  return invoke("dataset_export_submit", { request });
}
