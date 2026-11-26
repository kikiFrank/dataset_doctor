import { useEffect, useState } from "react";
import { cancelReview, openReview, saveReviewDraft } from "./api/review";
import type { AnnotationView, AssetSummary, CocoCategory, CocoImage, DisplayLocator, DraftReceipt, ReviewSession } from "./api/generated/wire-primitives";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";
import type { GeometryEvidence } from "./geometry-check-panel";
import { ReviewCanvas } from "./review-canvas";
import { makeDraft, seedBoxes } from "./review-document";
import type { BoxCoordinates, EditableBox } from "./review-document";

type Editor = {
  review: ReviewSession;
  categories: CocoCategory[];
  image: CocoImage;
  taskId: string;
  receipt: DraftReceipt | null;
  savedBoxes: EditableBox[];
  boxes: EditableBox[];
};

const coordinateNames = ["X", "Y", "宽", "高"];

export function ReviewPanel({ workspace, asset, annotation, media, evidence, onBlocked, onChanged, onImageError }: {
  workspace: OpenWorkspace;
  asset: AssetSummary;
  annotation: AnnotationView;
  media: DisplayLocator | null;
  evidence: GeometryEvidence | null;
  onBlocked: (blocked: boolean) => void;
  onChanged: () => void;
  onImageError: () => void;
}) {
  const [editor, setEditor] = useState<Editor | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const dirty = editor !== null && editor.boxes !== editor.savedBoxes;
  const blocked = dirty || busy;

  useEffect(() => {
    onBlocked(blocked);
    return () => onBlocked(false);
  }, [blocked, onBlocked]);

  const report = evidence?.report;
  const currentEvidence = report?.image_digest === annotation.image_digest
    && report.width > 0 && report.height > 0 ? evidence : null;
  const matchingImage = media?.resource_digest === annotation.image_digest;

  async function begin() {
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      const loaded = await openReview(workspace, asset.dataset_id, annotation);
      if (loaded.review.status !== "OPEN") throw new Error("此审核当前无法编辑。");
      const draft = loaded.draft;
      const source = draft?.coco_document ?? annotation.formal_coco;
      const proof = draft ? { image: draft.coco_document.images[0], taskId: draft.source_task_id }
        : currentEvidence && { taskId: currentEvidence.taskId, image: {
          id: source?.images[0]?.id ?? 1, file_name: asset.display_name,
          width: currentEvidence.report.width, height: currentEvidence.report.height,
        } };
      if (!proof) throw new Error("请先运行标注检查，取得当前图像尺寸。");
      const boxes = seedBoxes(source, loaded.categories);
      setEditor({
        review: loaded.review,
        categories: loaded.categories,
        ...proof,
        receipt: draft?.receipt ?? null,
        boxes,
        savedBoxes: boxes,
      });
      setSelected(boxes.length ? 0 : null);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  function update(index: number, box: EditableBox) {
    setEditor((current) => current && {
      ...current, boxes: current.boxes.map((original, position) => position === index ? box : original),
    });
    setNotice(null);
  }

  function draw(coordinates: BoxCoordinates) {
    if (!editor) return;
    const used = new Set(editor.boxes.map(({ id }) => id));
    let id = 1;
    while (used.has(id)) id += 1;
    const category = editor.categories.length === 1 ? editor.categories[0].id : null;
    const box: EditableBox = { id, bbox: coordinates, category_id: category, iscrowd: 0 };
    setEditor({ ...editor, boxes: [...editor.boxes, box] });
    setSelected(editor.boxes.length);
    setNotice(null);
  }

  async function save() {
    if (!editor || busy) return;
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      const document = makeDraft(editor.image, editor.categories, editor.boxes);
      const receipt = await saveReviewDraft(workspace, editor.review, editor.taskId, editor.receipt, document);
      setEditor({ ...editor, receipt, savedBoxes: editor.boxes,
        review: { ...editor.review, row_revision: receipt.review_revision } });
      setNotice("草稿已保存。");
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function cancel() {
    const review = editor?.review ?? annotation.active_review;
    if (!review || busy) return;
    setBusy(true);
    setError(null);
    try {
      await cancelReview(workspace, review);
      setEditor(null);
      setConfirmCancel(false);
      onChanged();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  const canCancel = editor || annotation.active_review?.status === "OPEN" || annotation.active_review?.status === "CONFLICT";
  const box = selected === null ? undefined : editor?.boxes[selected];
  return (
    <section className="review-panel" aria-label="人工审核">
      <h5>人工审核</h5>
      {!editor && <button type="button" disabled={busy || (!currentEvidence && !annotation.active_review)} onClick={() => void begin()}>
        {annotation.active_review ? "继续审核" : "开始审核"}
      </button>}
      {!editor && !currentEvidence && !annotation.active_review && <p>先运行标注检查，再审核或编辑标注。</p>}
      {editor && (
        <>
          <div className="review-layout">
            {media && matchingImage
              ? <ReviewCanvas image={editor.image} locator={media.locator} boxes={editor.boxes} selected={selected}
                  disabled={busy} onSelect={setSelected} onDraw={draw} onImageError={onImageError} />
              : <p>当前图像无法显示，请刷新样本。</p>}
            <div className="review-inspector">
              <label>标注框 <select value={selected ?? ""} disabled={busy} onChange={(event) => {
                setSelected(event.target.value === "" ? null : Number(event.target.value));
              }}>
                <option value="">选择框</option>
                {editor.boxes.map((item, index) => <option key={index} value={index}>
                  {index + 1}. {editor.categories.find((category) => category.id === item.category_id)?.name ?? "待选类别"}
                </option>)}
              </select></label>
              {box && selected !== null && (
                <fieldset disabled={busy}>
                  <legend>第 {selected + 1} 个框 · 原图像素</legend>
                  <label>类别 <select value={box.category_id ?? ""} onChange={(event) => {
                    update(selected, { ...box, category_id: event.target.value === "" ? null : Number(event.target.value) });
                  }}>
                    <option value="">选择类别</option>
                    {editor.categories.map(({ id, name }) => <option key={id} value={id}>{name}</option>)}
                  </select></label>
                  <div className="review-coordinates">
                    {coordinateNames.map((label, coordinate) => (
                      <label key={label}>{label}<input type="number" step="any"
                        value={Number.isFinite(box.bbox[coordinate]) ? box.bbox[coordinate] : ""} onChange={(event) => {
                          const bbox: BoxCoordinates = [...box.bbox];
                          bbox[coordinate] = event.currentTarget.valueAsNumber;
                          update(selected, { ...box, bbox });
                        }} />
                      </label>
                    ))}
                  </div>
                  <button type="button" onClick={() => {
                    setEditor({ ...editor, boxes: editor.boxes.filter((_, index) => index !== selected) });
                    setSelected(null);
                    setNotice(null);
                  }}>删除框</button>
                </fieldset>
              )}
            </div>
          </div>
          <div className="review-actions">
            <button type="button" disabled={busy || (!dirty && editor.receipt !== null)} onClick={() => void save()}>保存草稿</button>
            {dirty && <button type="button" disabled={busy} onClick={() => {
              setEditor({ ...editor, boxes: editor.savedBoxes });
              setSelected(null);
              setError(null);
            }}>放弃本次修改</button>}
          </div>
          {dirty && <p role="status">有未保存的修改。请保存草稿或放弃修改后切换样本或关闭工作区。</p>}
        </>
      )}
      {canCancel && !confirmCancel && <button type="button" disabled={busy} onClick={() => setConfirmCancel(true)}>取消审核</button>}
      {confirmCancel && <div className="review-actions">
        <p>取消审核会丢弃当前草稿，保留正式标注。</p>
        <button type="button" disabled={busy} onClick={() => void cancel()}>丢弃审核草稿</button>
        <button type="button" disabled={busy} onClick={() => setConfirmCancel(false)}>继续审核</button>
      </div>}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
    </section>
  );
}
