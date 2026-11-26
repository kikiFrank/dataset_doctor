import { acknowledgeWorkspaceVerification } from "./runtime";
import { getAnnotation, getDisplayLocator, listAssets, listDatasets } from "./dataset";
import { confirmImport, inspectImport, pickImportDirectory } from "./dataset-import";
import { exportDataset, selectExportDirectory } from "./dataset-export";
import { openReview, saveReviewDraft, submitReviewCommit } from "./review";
import { getTask, startGeometry } from "./task";
import { closeWorkspace, selectAndCreateWorkspace, selectAndOpenWorkspace, workspaceCommandError } from "./workspace";
import type { OpenWorkspace } from "./workspace";
import { makeDraft, seedBoxes } from "../review-document";
import { verifyWorkspaceColorPanel } from "./workspace-color-smoke";

const activeStates = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);

async function completed(workspace: OpenWorkspace, taskId: string) {
  const deadline = performance.now() + 10_000;
  do {
    const task = await getTask(workspace, taskId);
    if (task.status === "SUCCEEDED") return task;
    if (!activeStates.has(task.status)) throw new Error(task.error_code ?? task.status);
    await new Promise(resolve => setTimeout(resolve, 50));
  } while (performance.now() < deadline);
  throw new Error("Task did not finish in time");
}

export async function verifyWorkspaceBusinessIpc(verifyColor = false) {
  const name = "WebView2 Business Acceptance";
  let phase = "create Workspace";
  try {
    const workspace = await selectAndCreateWorkspace(name);
    if (!workspace || workspace.display_name !== name) throw new Error("Workspace identity mismatch");

    phase = "import inspection and registration";
    const directory = await pickImportDirectory(workspace);
    if (!directory) throw new Error("Import grant missing");
    const inspection = await inspectImport(workspace, directory.grant_id, "Signal Samples", crypto.randomUUID());
    const preview = (await completed(workspace, inspection.task_id)).preview;
    if (!preview?.can_confirm || preview.files.length !== 1) throw new Error("Invalid import preview");
    const imported = await confirmImport(workspace, preview, crypto.randomUUID());
    const registration = (await completed(workspace, imported.task_id)).registration;
    if (!registration || registration.asset_count !== "1") throw new Error("Invalid import receipt");
    const dataset = registration.dataset_id;
    const datasets = await listDatasets(workspace);
    const assets = await listAssets(workspace, dataset);
    if (datasets.items.length !== 1 || datasets.items[0].dataset_id !== dataset || assets.items.length !== 1) {
      throw new Error("Workspace did not return the registered dataset and asset");
    }
    const asset = assets.items[0].asset_id;

    phase = "display image and run actual geometry";
    const locator = await getDisplayLocator(workspace, asset);
    const image = new Image();
    image.src = locator.locator;
    await image.decode();
    if (image.naturalWidth !== 200 || image.naturalHeight !== 100) throw new Error("Image protocol returned incorrect dimensions");
    const check = await startGeometry(workspace, dataset, asset, crypto.randomUUID());
    const geometry = (await completed(workspace, check.task_id)).geometry_report;
    if (!geometry?.findings.some(finding => finding.code === "bbox_out_of_bounds")) {
      throw new Error("Worker did not report the original box outside image bounds");
    }

    phase = "review and confirm Final";
    const annotation = await getAnnotation(workspace, asset);
    if (!annotation.formal_coco || annotation.formal_coco.annotations[0].bbox[0] !== 190) {
      throw new Error("Incorrect original bounding box");
    }
    const editor = await openReview(workspace, dataset, annotation);
    const boxes = seedBoxes(annotation.formal_coco, editor.categories);
    if (boxes.length !== 1 || boxes[0].category_id !== editor.categories[0].id) {
      throw new Error("Editor did not map the source category to the registered category");
    }
    boxes[0].bbox = [30, 20, 40, 30];
    const fixed = makeDraft({
      ...annotation.formal_coco.images[0],
      width: geometry.width, height: geometry.height,
    }, editor.categories, boxes);
    const draft = await saveReviewDraft(workspace, editor.review, check.task_id, null, fixed);
    const committed = await submitReviewCommit(workspace, editor.review, draft, false, crypto.randomUUID());
    await completed(workspace, committed.task_id);
    const final = await getAnnotation(workspace, asset);
    if (final.annotation_state !== "CONFIRMED_POSITIVE" || final.formal_coco?.annotations[0].bbox[0] !== 30) {
      throw new Error("Confirmed result did not return the corrected bounding box");
    }

    phase = "export COCO";
    const destination = await selectExportDirectory(workspace);
    if (!destination) throw new Error("Export grant missing");
    const exportOperation = await exportDataset(workspace, dataset, destination.grant_id, "STRICT", crypto.randomUUID());
    const exported = (await completed(workspace, exportOperation.task_id)).coco_export;
    if (!exported || exported.image_count !== "1" || exported.annotation_count !== "1" || exported.excluded_count !== "0") {
      throw new Error("COCO export did not include the confirmed image and annotation");
    }

    phase = "reopen and read Final";
    if ((await closeWorkspace(workspace)).close_state !== "CLOSED") throw new Error("Workspace did not close");
    const reopened = await selectAndOpenWorkspace();
    if (!reopened || reopened.workspace_id !== workspace.workspace_id
        || reopened.workspace_session_id === workspace.workspace_session_id
        || (await getAnnotation(reopened, asset)).annotation_state !== "CONFIRMED_POSITIVE") {
      throw new Error("Final did not survive reopening the Workspace");
    }
    if (verifyColor) {
      phase = "apply a verified candidate through the color panel";
      const current = (await listAssets(reopened, dataset)).items.find(item => item.asset_id === asset);
      if (!current) throw new Error("Registered asset missing after reopen");
      await verifyWorkspaceColorPanel(reopened, current);
    }
    await closeWorkspace(reopened);
    await acknowledgeWorkspaceVerification();
  } catch (failure) {
    await acknowledgeWorkspaceVerification(`${phase}: ${workspaceCommandError(failure)}`);
  }
}
