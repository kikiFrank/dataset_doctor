import { createRoot } from "react-dom/client";
import { VideoImportPanel } from "../video-import-panel";
import { acknowledgeWorkspaceVerification } from "./runtime";
import { getVideo, listVideos } from "./video";
import {
  closeWorkspace, selectAndCreateWorkspace, selectAndOpenWorkspace, workspaceCommandError,
} from "./workspace";
import { waitFor } from "./workspace-workflow-smoke";

function enabledButton(parent: HTMLElement, label: string): HTMLButtonElement | null {
  return Array.from(parent.querySelectorAll<HTMLButtonElement>("button:enabled"))
    .find(button => button.textContent === label) ?? null;
}

export async function verifyWorkspaceVideoIpc() {
  try {
    const workspace = await selectAndCreateWorkspace("Synthetic video Workspace");
    if (!workspace) throw new Error("Cannot create the synthetic video Workspace");
    const host = document.body.appendChild(document.createElement("div"));
    const root = createRoot(host);
    root.render(<VideoImportPanel workspace={workspace} editGuard={{ blocked: false, onBlocked: () => {} }} />);
    try {
      (await waitFor(() => enabledButton(host, "选择视频"), host, "the video file action")).click();
      (await waitFor(() => enabledButton(host, "登记并探测"), host, "the video probe action")).click();
      await waitFor(() => host.textContent?.includes("视频已登记，帧索引可用。") ? host : null, host, "the real video index");
      (await waitFor(() => enabledButton(host, "Synthetic motion.mkv"), host, "the registered video")).click();
      const framePanel = await waitFor(
        () => host.querySelector<HTMLElement>('section[aria-label="精确帧定位"]'),
        host, "the precise frame panel",
      );
      const mode = framePanel.querySelector("select");
      if (!mode) throw new Error("The frame locator did not expose display indices");
      mode.value = "index";
      mode.dispatchEvent(new Event("change", { bubbles: true }));
      const index = await waitFor(() => framePanel.textContent?.includes("显示帧序号（从 0 开始）")
        ? framePanel.querySelector("input") : null, host, "the display frame index input");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(index, "2");
      index.dispatchEvent(new Event("input", { bubbles: true }));
      (await waitFor(() => enabledButton(framePanel, "读取视频帧"), host, "the precise frame action")).click();

      await waitFor(() => {
        const image = framePanel.querySelector<HTMLImageElement>('img[alt="视频帧 2"]');
        return image?.complete && image.naturalWidth === 32 && image.naturalHeight === 24 ? image : null;
      }, host, "display frame 2 decoded into a real WebView2 image");
      if (!framePanel.textContent?.includes("显示帧序号 2，实际时间 1000000 微秒")) {
        throw new Error("The displayed frame timestamp differs from the synthetic clip");
      }
    } finally {
      root.unmount();
      host.remove();
    }

    if ((await closeWorkspace(workspace)).close_state !== "CLOSED") throw new Error("Cannot close the video Workspace");
    const reopened = await selectAndOpenWorkspace();
    if (!reopened || reopened.workspace_id !== workspace.workspace_id || reopened.workspace_session_id === workspace.workspace_session_id) {
      throw new Error("The video Workspace did not reopen in a new session");
    }
    const videos = (await listVideos(reopened)).items;
    if (videos.length !== 1) throw new Error("Expected one registered video after reopening");
    const video = await getVideo(reopened, videos[0].asset_id);
    if (video.probe_status !== "SUCCEEDED" || video.source_availability !== "ONLINE"
        || video.streams.length !== 1 || video.streams[0].width !== 32 || video.streams[0].height !== 24) {
      throw new Error("The recognized video metadata did not survive reopening");
    }
    if ((await closeWorkspace(reopened)).close_state !== "CLOSED") throw new Error("Cannot close the reopened video Workspace");
    await acknowledgeWorkspaceVerification();
  } catch (failure) {
    await acknowledgeWorkspaceVerification("Packaged video: " + workspaceCommandError(failure));
  }
}
