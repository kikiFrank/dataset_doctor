import { useEffect, useState } from "react";
import { authorizeReviewWrite, cancelReview, openReview, saveReviewDraft, submitReviewCommit } from "./api/review";
import { annotationWriteGrantId, annotationWriteReady } from "./annotation-write-state";
import { getTask } from "./api/task";
import type {
  AnnotationView, AssetSummary, CocoCategory, CocoImage, DisplayLocator, DraftReceipt, ReviewSession, TrackObservationView,
  PathGrant, ReviewWriteAccessView,
} from "./api/generated/wire-primitives";
import { workspaceCommandError } from "./api/workspace";
import type { OpenWorkspace } from "./api/workspace";
import type { GeometryEvidence } from "./geometry-check-panel";
import { ReviewCanvas } from "./review-canvas";
import { adoptTrackingBox, makeDraft, nextBoxId, seedBoxes, trackSelections } from "./review-document";
import type { BoxCoordinates, EditableBox } from "./review-document";

type Editor = {
  writeAccess: ReviewWriteAccessView;
  review: ReviewSession;
  categories: CocoCategory[];
  image: CocoImage;
  taskId: string;
  receipt: DraftReceipt | null;
  savedBoxes: EditableBox[];
  boxes: EditableBox[];
};

const coordinateNames = ["X", "Y", "宽", "高"];

export function ReviewPanel({ workspace, asset, annotation, media, evidence, onBlocked, onChanged, onImageError, externalBlocked }: {
  workspace: OpenWorkspace;
  asset: AssetSummary;
  annotation: AnnotationView;
  media: DisplayLocator | null;
  evidence: GeometryEvidence | null;
  onBlocked: (blocked: boolean) => void;
  onChanged: () => void;
  onImageError: () => void;
  externalBlocked: boolean;
}) {
  const [editor, setEditor] = useState<Editor | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [negative, setNegative] = useState(false);
  const [approval, setApproval] = useState<{ key: string; negative: boolean } | null>(null);
  const [commitTask, setCommitTask] = useState<string | null>(null);
  const [writeGrant, setWriteGrant] = useState<PathGrant | null>(null);
  const [queryRetry, setQueryRetry] = useState(0);
  const dirty = editor !== null && editor.boxes !== editor.savedBoxes;
  const blocked = dirty || busy || commitTask !== null;
  const editingLocked = busy || approval !== null || externalBlocked;
  const writeReady = editor !== null && annotationWriteReady(editor.writeAccess, writeGrant);

  useEffect(() => { setWriteGrant(null); }, [workspace.workspace_session_id, asset.asset_id]);

  useEffect(() => {
    onBlocked(blocked);
    return () => onBlocked(false);
  }, [blocked, onBlocked]);

  useEffect(() => {
    if (!commitTask) return;
    const id = commitTask;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const task = await getTask(workspace, id);
        if (!active) return;
        if (task.status === "QUEUED" || task.status === "RUNNING") {
          timer = setTimeout(() => void poll(), 500);
          return;
        }
        setCommitTask(null);
        if (task.status === "SUCCEEDED") {
          setWriteGrant(null);
          setApproval(null);
          setEditor(null);
          onChanged();
        } else if (task.error_code === "FINAL_RESUME_REQUIRED") {
          setError("提交需要恢复。可以重试或重新打开工作区。");
        } else {
          setWriteGrant(null);
          setApproval(null);
          setEditor(null);
          setError("样本已变化，无法发布 Final。请刷新样本并处理冲突。");
        }
      } catch (failure) {
        if (active) setError(workspaceCommandError(failure));
      }
    }
    void poll();
    return () => { active = false; clearTimeout(timer); };
  }, [workspace, commitTask, queryRetry, onChanged]);

  const report = evidence?.report;
  const currentEvidence = report?.image_digest === annotation.image_digest
    && report.width > 0 && report.height > 0 ? evidence : null;
  const matchingImage = media?.resource_digest === annotation.image_digest;
  const candidates = annotation.tracking_candidates;

  async function begin() {
    if (externalBlocked) return;
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      if (!asset.dataset_id) throw new Error("请选择数据集中的图片。");
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
      const boxes = seedBoxes(source, loaded.categories, draft?.track_selections ?? loaded.initial_track_selections);
      setEditor({
        writeAccess: loaded.annotation_write_access,
        review: loaded.review,
        categories: loaded.categories,
        ...proof,
        receipt: draft?.receipt ?? null,
        boxes,
        savedBoxes: boxes,
      });
      setSelected(boxes.length ? 0 : null);
      setWriteGrant(null);
      setNegative(false);
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  function update(index: number, box: EditableBox) {
    if (editingLocked) return;
    setEditor((current) => current && {
      ...current, boxes: current.boxes.map((original, position) => position === index ? box : original),
    });
    setNotice(null);
  }

  function draw(coordinates: BoxCoordinates) {
    if (!editor || editingLocked) return;
    const id = nextBoxId(editor.boxes);
    const category = editor.categories.length === 1 ? editor.categories[0].id : null;
    const box: EditableBox = { id, bbox: coordinates, category_id: category, iscrowd: 0 };
    setEditor({ ...editor, boxes: [...editor.boxes, box] });
    setSelected(editor.boxes.length);
    setNotice(null);
  }

  function adopt(candidate: TrackObservationView) {
    if (!editor || editingLocked) return;
    const boxes = adoptTrackingBox(editor.boxes, candidate, editor.categories);
    if (boxes === editor.boxes) return;
    setEditor({ ...editor, boxes });
    setSelected(boxes.length - 1);
    setNotice(null);
  }

  async function save() {
    if (!editor || editingLocked) return;
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      const document = makeDraft(editor.image, editor.categories, editor.boxes);
      const selections = trackSelections(editor.boxes);
      const supplied = selections.length > 0 || editor.savedBoxes.some(box => box.track_id !== undefined);
      const receipt = await saveReviewDraft(workspace, editor.review, editor.taskId, editor.receipt,
        document, supplied ? selections : undefined);
      setEditor({ ...editor, receipt, savedBoxes: editor.boxes,
        review: { ...editor.review, row_revision: receipt.review_revision } });
      setNegative(false);
      setNotice("草稿已保存。");
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function authorizeWrite() {
    if (!editor || editingLocked) return;
    setBusy(true);
    setError(null);
    try {
      const grant = await authorizeReviewWrite(workspace, editor.review);
      if (grant) setWriteGrant(grant);
    } catch (failure) {
      setWriteGrant(null);
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function commit() {
    if (!editor?.receipt || dirty || busy || commitTask || externalBlocked) return;
    const empty = editor.boxes.length === 0;
    if (empty && !negative && !approval) return;
    let grantId: string | undefined;
    try {
      grantId = approval ? writeGrant?.grant_id : annotationWriteGrantId(editor.writeAccess, writeGrant);
    } catch (failure) {
      setWriteGrant(null);
      setError(workspaceCommandError(failure));
      return;
    }
    const decision = approval ?? { key: crypto.randomUUID(), negative: empty && negative };
    setApproval(decision);
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const task = await submitReviewCommit(workspace, editor.review, editor.receipt, decision.negative, decision.key, grantId);
      setCommitTask(task.task_id);
    } catch (failure) {
      if (failure && typeof failure === "object" && "code" in failure
        && (failure.code === "PERMISSION_DENIED" || failure.code === "FILE_BUSY")) {
        setApproval(null);
        setWriteGrant(null);
      }
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  async function cancel() {
    const review = editor?.review ?? annotation.active_review;
    if (!review || editingLocked) return;
    setBusy(true);
    setError(null);
    try {
      await cancelReview(workspace, review);
      setWriteGrant(null);
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
      {candidates?.status === "UNAVAILABLE" && <p role="status">无法核验该帧的追踪候选。</p>}
      {candidates?.status === "AVAILABLE" && <table aria-label={"帧 " + candidates.frame_index + " 的追踪候选"}>
        <thead><tr><th scope="col">轨迹</th><th scope="col">类别</th><th scope="col">框坐标 x、y、宽、高</th>
          {editor && <th scope="col">采用</th>}
        </tr></thead>
        <tbody>{candidates.observations.map(item => {
          const category = editor?.categories.find(entry => String(entry.id) === item.category_id);
          const adopted = editor?.boxes.some(box => box.track_id === item.track_id);
          return <tr key={item.track_id}>
            <td title={item.track_id}>{item.track_id.slice(0, 8)}</td>
            <td>{category?.name ?? item.category_id}</td>
            <td>{item.bbox_xywh.join(", ")}</td>
            {editor && <td><button type="button" disabled={editingLocked || !category || adopted}
              onClick={() => adopt(item)}>{adopted ? "已采用" : "采用此框"}</button></td>}
          </tr>;
        })}</tbody>
      </table>}
      {!editor && <button type="button" disabled={externalBlocked || busy || (!currentEvidence && !annotation.active_review)} onClick={() => void begin()}>
        {annotation.active_review ? "继续审核" : "开始审核"}
      </button>}
      {!editor && !currentEvidence && !annotation.active_review && <p>先运行标注检查，再审核或编辑标注。</p>}
      {editor && (
        <>
          <div className="review-layout">
            {media && matchingImage
              ? <ReviewCanvas image={editor.image} locator={media.locator} boxes={editor.boxes} selected={selected}
                  disabled={editingLocked} onSelect={setSelected} onDraw={draw}
                  onMove={(index, bbox) => update(index, { ...editor.boxes[index], bbox })} onImageError={onImageError} />
              : <p>当前图像无法显示，请刷新样本。</p>}
            <div className="review-inspector">
              <label>标注框 <select value={selected ?? ""} onChange={(event) => {
                setSelected(event.target.value === "" ? null : Number(event.target.value));
              }}>
                <option value="">选择框</option>
                {editor.boxes.map((item, index) => <option key={index} value={index}>
                  {index + 1}. {editor.categories.find((category) => category.id === item.category_id)?.name ?? "待选类别"}
                </option>)}
              </select></label>
              {box && selected !== null && (
                <fieldset disabled={editingLocked}>
                  <legend>第 {selected + 1} 个框 · 原图像素</legend>
                  <label>类别 <select value={box.category_id ?? ""} onChange={(event) => {
                    const next = event.target.value === "" ? null : Number(event.target.value);
                    update(selected, { ...box, category_id: next, track_id: next === box.category_id ? box.track_id : undefined });
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
                  {box.track_id && <button type="button" onClick={() => update(selected, { ...box, track_id: undefined })}>
                    取消轨迹关联
                  </button>}
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
            {!writeReady && !approval && <p role="status">
              {editor.writeAccess.read_only ? "标注文件只读，调整权限后可重新授权。" : "正式标注写入待授权。"}
            </p>}
            {!approval && (!writeReady || editor.writeAccess.authorization_required) && <button type="button"
              disabled={editingLocked} onClick={() => void authorizeWrite()}>授权标注写入</button>}
            {writeGrant && writeReady && <p>本次授权目录 {writeGrant.display_path}</p>}
            <button type="button" disabled={editingLocked || (!dirty && editor.receipt !== null)} onClick={() => void save()}>保存草稿</button>
            {dirty && <button type="button" disabled={editingLocked} onClick={() => {
              setEditor({ ...editor, boxes: editor.savedBoxes });
              setSelected(null);
              setError(null);
            }}>放弃本次修改</button>}
            {editor.receipt && !dirty && editor.boxes.length === 0 && (
              <label><input type="checkbox" checked={negative} disabled={editingLocked}
                onChange={(event) => setNegative(event.target.checked)} /> 确认这张图是负样本</label>
            )}
            {editor.receipt && !dirty && <button type="button"
              disabled={externalBlocked || busy || commitTask !== null || (!approval && !writeReady) || (!approval && editor.boxes.length === 0 && !negative)}
              onClick={() => void commit()}>{approval && !commitTask ? "重试提交" : "确认并写入 Final"}</button>}
          </div>
          {dirty && <p role="status">有未保存的修改。请保存草稿或放弃修改后切换样本或关闭工作区。</p>}
        </>
      )}
      {canCancel && !confirmCancel && <button type="button" disabled={editingLocked} onClick={() => setConfirmCancel(true)}>取消审核</button>}
      {confirmCancel && <div className="review-actions">
        <p>取消审核会丢弃当前草稿，保留正式标注。</p>
        <button type="button" disabled={externalBlocked || busy} onClick={() => void cancel()}>丢弃审核草稿</button>
        <button type="button" disabled={busy} onClick={() => setConfirmCancel(false)}>继续审核</button>
      </div>}
      {error && <p role="alert">{error}</p>}
      {commitTask && !error && <p role="status">正在发布 Final…</p>}
      {commitTask && error && <button type="button" onClick={() => { setError(null); setQueryRetry((value) => value + 1); }}>重试查询</button>}
      {notice && <p role="status">{notice}</p>}
    </section>
  );
}
