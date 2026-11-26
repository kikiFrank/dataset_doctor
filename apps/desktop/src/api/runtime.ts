import { invoke } from "@tauri-apps/api/core";

interface RuntimeStatus {
  version: string;
  verify_workspace: boolean;
}

export function acknowledgeDesktopRuntime(): Promise<RuntimeStatus> {
  const webview = window as Window & { chrome?: { webview?: unknown } };
  return invoke<RuntimeStatus>("desktop_ready", {
    request: { webview2: webview.chrome?.webview != null },
  });
}

export function acknowledgeWorkspaceVerification(): Promise<void> {
  return invoke("desktop_workspace_verified");
}
