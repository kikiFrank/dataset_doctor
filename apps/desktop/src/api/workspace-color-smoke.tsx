import { createRoot } from "react-dom/client";
import { ColorRepairPanel } from "../color-repair-panel";
import { getAnnotation, getDisplayLocator } from "./dataset";
import type { AssetSummary } from "./generated/wire-primitives";
import { getTask, startGeometry } from "./task";
import type { OpenWorkspace } from "./workspace";

const activeStates = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);

async function waitFor<T>(read: () => T | null, description: string, failure?: () => string | null): Promise<T> {
  const deadline = performance.now() + 10_000;
  do {
    const value = read();
    if (value !== null) return value;
    const error = failure?.();
    if (error) throw new Error(error);
    await new Promise(resolve => setTimeout(resolve, 30));
  } while (performance.now() < deadline);
  throw new Error("Timed out waiting for " + description);
}

async function geometry(workspace: OpenWorkspace, dataset: string, asset: string) {
  const operation = await startGeometry(workspace, dataset, asset, crypto.randomUUID());
  const deadline = performance.now() + 10_000;
  do {
    const task = await getTask(workspace, operation.task_id);
    if (task.status === "SUCCEEDED" && task.geometry_report) {
      return { taskId: task.task_id, report: task.geometry_report };
    }
    if (!activeStates.has(task.status)) throw new Error(task.error_code ?? task.status);
    await new Promise(resolve => setTimeout(resolve, 40));
  } while (performance.now() < deadline);
  throw new Error("Color acceptance geometry did not finish");
}

export async function verifyWorkspaceColorPanel(workspace: OpenWorkspace, asset: AssetSummary) {
  if (!asset.dataset_id) throw new Error("Color acceptance asset has no dataset");
  const before = await getAnnotation(workspace, asset.asset_id);
  const media = await getDisplayLocator(workspace, asset.asset_id);
  const check = await geometry(workspace, asset.dataset_id, asset.asset_id);
  if (check.report.image_digest !== before.image_digest || before.annotation_state !== "CONFIRMED_POSITIVE") {
    throw new Error("Color acceptance did not start from the confirmed input");
  }

  const host = document.body.appendChild(document.createElement("div"));
  const root = createRoot(host);
  let imageError: string | null = null;
  const currentError = () => imageError ?? host.querySelector('[role="alert"]')?.textContent ?? null;
  let changed = false;
  root.render(<ColorRepairPanel workspace={workspace} asset={asset} annotation={before} media={media}
    evidence={check} editing={false} onBusy={() => {}} onChanged={() => { changed = true; }}
    onImageError={() => { imageError = "Original image failed to display"; }} />);
  try {
    const button = (text: string) => waitFor(() =>
      Array.from(host.querySelectorAll("button")).find(element =>
        !element.disabled && element.textContent?.trim() === text
      ) ?? null, text, currentError);
    const method = await waitFor(() => Array.from(host.querySelectorAll("select")).find(element =>
      Array.from(element.options).some(option => option.value === "SOLID_FILL")
    ) ?? null, "the real color method selector");
    method.value = "SOLID_FILL";
    method.dispatchEvent(new Event("change", { bubbles: true }));
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    if (!setter) throw new Error("Cannot set the native numeric inputs");
    for (const size of ["宽", "高"]) {
      const input = await waitFor(() => {
        const label = Array.from(host.querySelectorAll("label")).find(element => element.textContent?.trim() === size);
        return label?.querySelector("input") ?? null;
      }, size, currentError);
      setter.call(input, "1");
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await new Promise(resolve => setTimeout(resolve, 0));
    }
    (await button("生成颜色预览")).click();
    const mask = await waitFor(() => {
      const element = host.querySelector<HTMLImageElement>('img[alt="实际改色区域掩膜"]');
      return element?.complete && element.naturalWidth === 200 ? element : null;
    }, "the decoded mask", currentError);
    if (mask.naturalHeight !== 100) throw new Error("Color mask height differs from the source");
    (await button("应用此候选")).click();
    const approve = await button("确认替换图片");
    if ((await getAnnotation(workspace, asset.asset_id)).image_digest !== before.image_digest) {
      throw new Error("The original was replaced before confirmation");
    }
    approve.click();
    await waitFor(() => changed ? true : null, "the committed color application", currentError);
  } finally {
    root.unmount();
    host.remove();
  }

  const after = await getAnnotation(workspace, asset.asset_id);
  if (after.image_digest === before.image_digest || after.annotation_digest !== before.annotation_digest
      || after.annotation_state !== "STALE") {
    throw new Error("Color application did not preserve COCO and invalidate the old review");
  }
  const rechecked = await geometry(workspace, asset.dataset_id, asset.asset_id);
  if (rechecked.report.image_digest !== after.image_digest || rechecked.report.findings.length !== 0) {
    throw new Error("Real post-apply geometry did not use the new image");
  }
}
