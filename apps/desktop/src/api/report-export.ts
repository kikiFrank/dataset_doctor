import { invoke } from "@tauri-apps/api/core";
import type {
  ArtifactReference, ImportOperationRef, ReportExportFormat, ReportExportSubmitRequest,
} from "./generated/wire-primitives";
import { context, type OpenWorkspace } from "./workspace";

export function exportReport(
  workspace: OpenWorkspace, report: ArtifactReference, format: ReportExportFormat, grantId: string,
): Promise<ImportOperationRef> {
  const request: ReportExportSubmitRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: crypto.randomUUID() },
    report_ref: report,
    format,
    destination_grant_id: grantId,
  };
  return invoke("report_export_submit", { request });
}
