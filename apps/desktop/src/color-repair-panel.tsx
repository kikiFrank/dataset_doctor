import { useEffect, useRef, useState } from "react";
import { applyColor, previewColor } from "./api/color";
import { getTaskImageLocator } from "./api/dataset";
import type { AnnotationView, AssetSummary, ColorPreviewView, DisplayLocator } from "./api/generated/wire-primitives";
import { getTask } from "./api/task";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { canApproveColor, colorIntent, type ColorForm } from "./color-repair";
import type { GeometryEvidence } from "./geometry-check-panel";
import { ReviewCanvas } from "./review-canvas";

const activeStates = new Set(["QUEUED", "DISPATCHING", "RUNNING", "CANCELLING"]);
const failures: Record<string, string> = {
  INPUT_CHANGED: "图片或标注已变化，请刷新样本并重新预览。",
  invalid_input: "颜色参数或区域不适用，请核对后重试。",
  unsupported_format: "此图像格式不支持颜色修复。",
  WORKER_START_FAILED: "颜色 Worker 无法启动，请检查本机运行环境。",
  SESSION_ENDED: "原工作区会话已关闭，请重新预览。",
  COLOR_APPLY_RESUME_REQUIRED: "应用尚需恢复。可重试本次应用或重新打开工作区。",
};
const coordinates = ["X", "Y", "宽", "高"];

type Pending = { id: string; kind: "preview"; intent: string } | { id: string; kind: "apply" };
type Shown = { preview: ColorPreviewView; candidate: DisplayLocator; mask: DisplayLocator; intent: string };

export function ColorRepairPanel({
  workspace, asset, annotation, media, evidence, editing, onBusy, onChanged, onImageError,
}: {
  workspace: OpenWorkspace;
  asset: AssetSummary;
  annotation: AnnotationView;
  media: DisplayLocator | null;
  evidence: GeometryEvidence | null;
  editing: boolean;
  onBusy: (busy: boolean) => void;
  onChanged: () => void;
  onImageError: () => void;
}) {
  const [form, setForm] = useState<ColorForm>({
    shape: "RECT", bbox: ["0", "0", "", ""], points: "", method: "HUE_REMAP",
    target: "#1aae66", strength: "1", feather: "0", hueGate: false, hueMin: "0", hueMax: "359",
  });
  const [pending, setPending] = useState<Pending | null>(null);
  const [submitting, setSubmitting] = useState<"preview" | "apply" | null>(null);
  const [shown, setShown] = useState<Shown | null>(null);
  const [renderedDigest, setRenderedDigest] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const keys = useRef({ preview: { intent: "", key: "" }, apply: { artifact: "", key: "" } });
  const processing = pending !== null || submitting !== null;
  const applying = pending?.kind === "apply" || submitting === "apply";
  const report = evidence?.report;
  const readyImage = report?.image_digest === annotation.image_digest && report.width > 0 && report.height > 0
    && media?.resource_digest === annotation.image_digest ? { width: report.width, height: report.height } : null;
  const dataset = asset.dataset_id;
  const scope = dataset ? {
    dataset_id: dataset, asset_id: asset.asset_id, base_image_digest: annotation.image_digest,
  } : null;
  let intent = "";
  try {
    if (readyImage) intent = JSON.stringify(colorIntent(form, readyImage));
  } catch {
    // Report validation details when the user submits the form.
  }
  const displayed = shown?.intent === intent ? shown : null;
  const approved = displayed && scope && canApproveColor({ ...displayed, renderedDigest }, scope, intent);

  useEffect(() => {
    onBusy(applying);
    return () => onBusy(false);
  }, [applying, onBusy]);

  useEffect(() => {
    if (!pending) return;
    const request = pending;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const task = await getTask(workspace, request.id);
        if (!current) return;
        if (activeStates.has(task.status)) {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        if (task.status === "SUCCEEDED" && request.kind === "preview" && task.color_preview) {
          const preview = task.color_preview;
          const [candidate, mask] = await Promise.all([
            getTaskImageLocator(workspace, preview.candidate_ref),
            getTaskImageLocator(workspace, preview.mask_ref),
          ]);
          if (!current) return;
          if (candidate.resource_digest !== preview.candidate_ref.sha256 || mask.resource_digest !== preview.mask_ref.sha256) {
            throw new Error("无法核验显示的候选和掩膜。");
          }
          setShown({ preview, candidate, mask, intent: request.intent });
          keys.current.preview = { intent: "", key: "" };
        } else if (task.status === "SUCCEEDED" && request.kind === "apply" && task.color_apply) {
          onChanged();
        } else {
          setError(failures[task.error_code ?? ""] ?? "颜色任务未完成，请核对图像和参数。");
        }
        setPending(null);
      } catch (failure) {
        if (current) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { current = false; clearTimeout(timer); };
  }, [workspace, pending, retry, onChanged]);

  function update(change: Partial<ColorForm>) {
    setForm(current => ({ ...current, ...change }));
    setConfirm(false);
  }

  async function submit() {
    if (!readyImage || !scope || processing || editing) return;
    setError(null);
    try {
      const config = colorIntent(form, readyImage);
      const fingerprint = JSON.stringify(config);
      if (keys.current.preview.intent !== fingerprint) {
        keys.current.preview = { intent: fingerprint, key: crypto.randomUUID() };
      }
      setSubmitting("preview");
      setShown(null);
      setRenderedDigest(null);
      setConfirm(false);
      const operation = await previewColor(workspace, { ...scope, ...config }, keys.current.preview.key);
      setPending({ id: operation.task_id, kind: "preview", intent: fingerprint });
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setSubmitting(null);
    }
  }

  async function approve() {
    if (!displayed || !approved || !scope || !confirm || editing || processing) return;
    const { preview } = displayed;
    const candidate = preview.candidate_ref;
    if (keys.current.apply.artifact !== candidate.artifact_id) {
      keys.current.apply = { artifact: candidate.artifact_id, key: crypto.randomUUID() };
    }
    setSubmitting("apply");
    setError(null);
    try {
      const operation = await applyColor(workspace, {
        ...scope, candidate_ref: candidate, expected_asset_revision: preview.asset_revision,
      }, keys.current.apply.key);
      setPending({ id: operation.task_id, kind: "apply" });
      setConfirm(false);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setSubmitting(null);
    }
  }

  return <section className="color-repair" aria-label="颜色修复">
    <h5>颜色修复</h5>
    {!readyImage && <p>先运行标注检查以读取当前图像的实际尺寸。</p>}
    {readyImage && media && <>
      <form onSubmit={event => { event.preventDefault(); void submit(); }}>
        <fieldset disabled={editing || processing}>
          <label>修复区域 <select value={form.shape} onChange={event => update({ shape: event.target.value as ColorForm["shape"] })}>
            <option value="RECT">矩形</option><option value="POLYGON">多边形</option>
          </select></label>
          {form.shape === "RECT" ? <div className="color-fields">
            {coordinates.map((label, index) => <label key={label}>{label}<input type="number" step="any"
              value={form.bbox[index]} onChange={event => {
                const bbox: ColorForm["bbox"] = [...form.bbox];
                bbox[index] = event.target.value;
                update({ bbox });
              }} /></label>)}
            <button type="button" onClick={() => update({ bbox: ["0", "0", String(readyImage.width), String(readyImage.height)] })}>选取整图</button>
          </div> : <label>顶点（每行 x,y） <textarea rows={4} value={form.points}
            onChange={event => update({ points: event.target.value })} /></label>}
          <div className="color-fields">
            <label>方法 <select value={form.method} onChange={event => update({ method: event.target.value as ColorForm["method"] })}>
              <option value="HUE_REMAP">调整色相</option><option value="SOLID_FILL">纯色填充</option>
            </select></label>
            <label>目标颜色 <input type="color" value={form.target} onChange={event => update({ target: event.target.value })} /></label>
            <label>强度 <input type="number" step="0.01" min="0" max="1" value={form.strength}
              onChange={event => update({ strength: event.target.value })} /></label>
            <label>羽化像素 <input type="number" step="any" min="0" max="64" value={form.feather}
              onChange={event => update({ feather: event.target.value })} /></label>
          </div>
          <label><input type="checkbox" checked={form.hueGate} onChange={event => update({ hueGate: event.target.checked })} /> 限制原图色相</label>
          {form.hueGate && <div className="color-fields">
            <label>起始角度 <input type="number" step="any" value={form.hueMin} onChange={event => update({ hueMin: event.target.value })} /></label>
            <label>结束角度 <input type="number" step="any" value={form.hueMax} onChange={event => update({ hueMax: event.target.value })} /></label>
          </div>}
          <button type="submit">生成颜色预览</button>
        </fieldset>
      </form>
      <div className="color-comparison">
        <figure><figcaption>原图{form.shape === "RECT" && " · 可拖出矩形"}</figcaption>
          <ReviewCanvas image={{ id: 1, file_name: asset.display_name, ...readyImage }} locator={media.locator}
            boxes={[]} selected={null} disabled={editing || processing || form.shape !== "RECT"}
            onSelect={() => {}} onImageError={onImageError}
            onDraw={bbox => update({ bbox: [String(bbox[0]), String(bbox[1]), String(bbox[2]), String(bbox[3])] })} />
        </figure>
        {displayed && <>
          <figure><figcaption>候选</figcaption>
            <img key={displayed.preview.candidate_ref.artifact_id} src={displayed.candidate.locator} alt="颜色修复候选"
              onLoad={() => setRenderedDigest(displayed.candidate.resource_digest)}
              onError={() => { setRenderedDigest(null); setError("候选图像无法显示，请重新预览。"); }} />
          </figure>
          <figure><figcaption>掩膜</figcaption><img src={displayed.mask.locator} alt="实际改色区域掩膜" /></figure>
        </>}
      </div>
      {displayed && <>
        <p>改动 {displayed.preview.changed_pixels} 个像素。{displayed.preview.lossy_reencode && "JPEG 重编码可能改变区域外像素。"}</p>
        <button type="button" disabled={!approved || processing || editing} onClick={() => setConfirm(true)}>应用此候选</button>
        {confirm && approved && <div role="group" aria-label="确认应用颜色候选">
          <p>应用后将替换当前图片，并使已有检查和审核过期。</p>
          <button type="button" disabled={processing || editing} onClick={() => void approve()}>确认替换图片</button>
          <button type="button" disabled={processing} onClick={() => setConfirm(false)}>取消</button>
        </div>}
      </>}
    </>}
    {pending && !error && <p role="status">{applying ? "正在应用颜色候选…" : "正在生成颜色候选…"}</p>}
    {error && <p role="alert">{error}</p>}
    {pending && error && <button type="button" onClick={() => { setError(null); setRetry(value => value + 1); }}>重试查询</button>}
  </section>;
}
