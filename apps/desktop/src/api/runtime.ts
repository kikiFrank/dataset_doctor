import { invoke } from "@tauri-apps/api/core";

interface RuntimeStatus {
  version: string;
  verify_workspace: boolean;
  verify_workspace_create: boolean;
  verify_workspace_business: boolean;
  verify_workspace_color: boolean;
  verify_workspace_workflow: boolean;
  verify_workspace_workflow_controls: boolean;
  verify_workspace_report_export: boolean;
  verify_workspace_staging_batch: boolean;
  verify_workspace_video: boolean;
}

export function acknowledgeDesktopRuntime(): Promise<RuntimeStatus> {
  const webview = window as Window & { chrome?: { webview?: unknown } };
  return invoke<RuntimeStatus>("desktop_ready", {
    request: { webview2: webview.chrome?.webview != null },
  });
}

export function acknowledgeWorkspaceVerification(failure?: string): Promise<void> {
  return invoke("desktop_workspace_verified", { failure });
}
