import { invoke } from "@tauri-apps/api/core";
import type {
  DatasetImportExternalRequest, DatasetImportInspectRequest, DatasetImportPreview,
  ImportOperationRef, ImportTaskView, PathGrant, PathSelectRequest, TaskGetRequest,
} from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function pickImportDirectory(workspace: OpenWorkspace): Promise<PathGrant | null> {
  const requestId = crypto.randomUUID();
  const request: PathSelectRequest = {
    request_id: requestId,
    context: context(workspace, requestId),
    purpose: "IMPORT",
    kind: "DIRECTORY",
    access: "READ",
  };
  return invoke("path_select", { request });
}

export function inspectImport(
  workspace: OpenWorkspace, grantId: string, name: string, idempotencyKey: string,
): Promise<ImportOperationRef> {
  const request: DatasetImportInspectRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    root_grant_id: grantId,
    dataset_name: name,
    format: "COCO_IMAGE_BBOX",
  };
  return invoke("dataset_import_inspect", { request });
}

export function confirmImport(
  workspace: OpenWorkspace, preview: DatasetImportPreview, idempotencyKey: string,
): Promise<ImportOperationRef> {
  const request: DatasetImportExternalRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    import_plan_ref: preview.plan_id,
    plan_fingerprint: preview.plan_fingerprint,
  };
  return invoke("dataset_import_external", { request });
}

export function getImportTask(workspace: OpenWorkspace, taskId: string): Promise<ImportTaskView> {
  const request: TaskGetRequest = { context: context(workspace, crypto.randomUUID()), task_id: taskId };
  return invoke("task_get", { request });
}
