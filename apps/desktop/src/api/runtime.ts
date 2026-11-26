import { invoke } from "@tauri-apps/api/core";

export function acknowledgeDesktopRuntime(): Promise<string> {
  const webview = window as Window & { chrome?: { webview?: unknown } };
  return invoke<string>("desktop_ready", {
    request: { webview2: webview.chrome?.webview != null },
  });
}
