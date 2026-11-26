import { invoke } from "@tauri-apps/api/core";
import type {
  WorkflowGetRequest, WorkflowListRequest, WorkflowPageView, WorkflowReportGetRequest,
  WorkflowReportView, WorkflowStartRequest, WorkflowView,
} from "./generated/wire-primitives";
import { context, type OpenWorkspace } from "./workspace";

export function startWorkflow(
  workspace: OpenWorkspace, datasetId: string, sampleLimit: number, key: string,
): Promise<WorkflowView> {
  const request: WorkflowStartRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: key },
    scope: { dataset_id: datasetId },
    template_id: "industrial_dataset_healthcheck.v1",
    policy_overrides: { sample_limit: sampleLimit },
  };
  return invoke("workflow_start", { request });
}

export function getWorkflow(workspace: OpenWorkspace, runId: string): Promise<WorkflowView> {
  const request: WorkflowGetRequest = {
    context: context(workspace, crypto.randomUUID()), workflow_run_id: runId,
  };
  return invoke("workflow_get", { request });
}

export function listWorkflows(workspace: OpenWorkspace, datasetId: string, cursor?: string): Promise<WorkflowPageView> {
  const request: WorkflowListRequest = {
    context: context(workspace, crypto.randomUUID()), dataset_id: datasetId, page_size: 12, cursor,
  };
  return invoke("workflow_list", { request });
}

export function getWorkflowReport(workspace: OpenWorkspace, runId: string): Promise<WorkflowReportView> {
  const request: WorkflowReportGetRequest = {
    context: context(workspace, crypto.randomUUID()), workflow_run_id: runId,
  };
  return invoke("report_get", { request });
}
