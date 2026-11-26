import { useEffect, useRef, useState } from "react";
import { getDataset, listDatasets } from "./api/dataset";
import type {
  ArtifactReference, CocoDocument, DatasetDetail, DatasetPage, DisplayLocator,
  VideoDetail, VideoFrame, VideoSegment,
} from "./api/generated/wire-primitives";
import { getKeyframeDraft, saveKeyframeDraft, saveSegment } from "./api/video";
import { workspaceCommandError, type OpenWorkspace } from "./api/workspace";
import { ReviewCanvas } from "./review-canvas";
import { makeDraft, seedBoxes, type EditableBox } from "./review-document";
import { keyframeChoices, locateDraft, segmentInput } from "./video-annotation";

export type VideoEditGuard = { blocked: boolean; onBlocked: (blocked: boolean) => void };

type AnnotationProps = {
  workspace: OpenWorkspace;
  detail: VideoDetail;
  frame: VideoFrame | null;
  image: DisplayLocator | null;
  disabled: boolean;
  editGuard: VideoEditGuard;
  onChanged: () => void;
  onImageError: () => void;
};

export function VideoAnnotationPanel(props: AnnotationProps) {
  const { workspace, detail, frame, image } = props;
  const [page, setPage] = useState<DatasetPage | null>(null);
  const [target, setTarget] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [selectedDataset, setSelectedDataset] = useState<DatasetDetail | null>(null);

  useEffect(() => {
    let active = true;
    void listDatasets(workspace).then(value => {
      if (active) setPage(value);
    }).catch(failure => { if (active) setError(workspaceCommandError(failure)); });
    return () => { active = false; };
  }, [workspace]);

  useEffect(() => {
    if (!target) return;
    let active = true;
    setError(null);
    void getDataset(workspace, target).then(value => {
      if (active) setSelectedDataset(value);
    }).catch(failure => { if (active) setError(workspaceCommandError(failure)); });
    return () => { active = false; };
  }, [workspace, target, detail.row_revision]);

  async function more() {
    if (!page?.next_cursor) return;
    setLoading(true);
    setError(null);
    try {
      const next = await listDatasets(workspace, page.next_cursor);
      setPage({ items: [...page.items, ...next.items], next_cursor: next.next_cursor });
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setLoading(false);
    }
  }

  const dataset = selectedDataset?.dataset_id === target ? selectedDataset : null;
  const manifest = detail.video_annotation_manifest;
  const savedContext = manifest?.target_contexts[target];
  const changed = dataset && savedContext && dataset.label_schema_digest !== savedContext.label_schema_digest;
  const draft = frame && dataset ? locateDraft(manifest, target, frame) : null;
  const segments = manifest?.segments.filter(segment => segment.target_dataset_id === target) ?? [];
  const renderEditor = frame && image && dataset && !changed;
  return <section aria-label="视频标注">
    <h3>视频标注</h3>
    <label>目标数据集 <select value={target} disabled={props.editGuard.blocked} onChange={event => setTarget(event.target.value)}>
      <option value="">选择数据集</option>
      {page?.items.map(item => <option key={item.dataset_id} value={item.dataset_id}>{item.name}</option>)}
    </select></label>
    {page?.next_cursor && <button type="button" disabled={loading} onClick={() => void more()}>加载更多数据集</button>}
    {page?.items.length === 0 && <p>请先导入带有类别的数据集。</p>}
    {error && <p role="alert">{error}</p>}
    {changed && <p role="alert">目标数据集的类别已变化，已有视频标注需要重新核对。</p>}
    {frame && image && !renderEditor && <img src={image.locator} width={frame.width} height={frame.height}
      alt={"视频帧 " + frame.frame_index} onError={props.onImageError} />}
    {renderEditor && <KeyframeEditor key={dataset.label_schema_digest + target + frame.image_ref.artifact_id + (draft?.artifact_id ?? "")}
      {...props} frame={frame} image={image} dataset={dataset} draft={draft} />}
    {dataset && <SegmentEditor key={target} {...props} dataset={dataset}
      disabled={props.disabled || props.editGuard.blocked || Boolean(changed)} segments={segments} />}
  </section>;
}

function KeyframeEditor({ workspace, detail, frame, image, dataset, draft, disabled, editGuard, onChanged, onImageError }: AnnotationProps & {
  frame: VideoFrame;
  image: DisplayLocator;
  dataset: DatasetDetail;
  draft: ArtifactReference | null;
}) {
  const [source, setSource] = useState<CocoDocument | null>(null);
  const [savedBoxes, setSavedBoxes] = useState<EditableBox[]>([]);
  const [boxes, setBoxes] = useState<EditableBox[]>(savedBoxes);
  const [selected, setSelected] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [category, setCategory] = useState<number | null>(null);
  const initial = useRef({ draft, categories: dataset.categories });
  const dirty = boxes !== savedBoxes;
  const { onBlocked } = editGuard;

  useEffect(() => {
    onBlocked(dirty || busy);
    return () => onBlocked(false);
  }, [dirty, busy, onBlocked]);

  useEffect(() => {
    const { draft, categories } = initial.current;
    if (!draft) return;
    let active = true;
    void getKeyframeDraft(workspace, detail.source_video_id, draft).then(document => {
      if (active) {
        setSource(document);
        const restored = seedBoxes(document, categories);
        setSavedBoxes(restored);
        setBoxes(restored);
      }
    }).catch(failure => { if (active) setError(workspaceCommandError(failure)); });
    return () => { active = false; };
  }, [workspace, detail.source_video_id]);

  const canvasImage = source?.images[0] ?? { id: 1, file_name: "frame.png", width: frame.width, height: frame.height };
  const locked = disabled || busy || Boolean(draft && !source);
  const selectedCategory = category ?? dataset.categories[0]?.id ?? null;
  async function save() {
    if (locked) return;
    setBusy(true);
    setError(null);
    try {
      await saveKeyframeDraft(workspace, {
        source_video_id: detail.source_video_id, target_dataset_id: dataset.dataset_id,
        frame_ref: frame, coco_document: makeDraft(canvasImage, dataset.categories, boxes),
        expected_video_revision: detail.row_revision,
      });
      setSavedBoxes(boxes);
      onChanged();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return <section aria-label="当前帧标注">
    <label>绘制类别 <select disabled={locked || dataset.categories.length === 0}
      value={selectedCategory ?? ""} onChange={event => setCategory(Number(event.target.value))}>
      {dataset.categories.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}
    </select></label>
    <ReviewCanvas image={canvasImage} locator={image.locator} boxes={boxes} selected={selected}
      disabled={locked || selectedCategory === null} onSelect={setSelected} onImageError={onImageError}
      onDraw={bbox => {
        const id = boxes.reduce((max, box) => Math.max(max, box.id), 0) + 1;
        setBoxes([...boxes, { id, bbox, category_id: selectedCategory, iscrowd: 0 }]);
        setSelected(boxes.length);
      }} />
    <button type="button" disabled={locked || selected === null} onClick={() => {
      setBoxes(boxes.filter((_, index) => index !== selected));
      setSelected(null);
    }}>移除选中框</button>
    <button type="button" disabled={locked} onClick={() => void save()}>保存人工关键帧</button>
    {dirty && <button type="button" disabled={busy} onClick={() => {
      setBoxes(savedBoxes);
      setSelected(null);
      setError(null);
    }}>放弃未保存修改</button>}
    {draft && source && <p role="status">已读取保存的关键帧草稿。</p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}

function SegmentEditor({ workspace, detail, dataset, segments, disabled, onChanged }: AnnotationProps & {
  dataset: DatasetDetail;
  segments: VideoSegment[];
}) {
  const [id, setId] = useState("");
  const [expected, setExpected] = useState("0");
  const [start, setStart] = useState("0");
  const [end, setEnd] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const choices = keyframeChoices(detail.video_annotation_manifest, dataset.dataset_id);

  function edit(segment: VideoSegment | undefined) {
    setId(segment?.segment_id ?? "");
    setExpected(segment?.revision ?? "0");
    setStart(segment?.start_us ?? "0");
    setEnd(segment?.end_us ?? "");
    setSelected(segment?.keyframes.map(keyframe => keyframe.annotation_ref.artifact_id) ?? []);
    setError(null);
    setNotice(null);
  }

  async function save() {
    if (disabled || busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const keyframes = choices.filter(keyframe => selected.includes(keyframe.annotation_ref.artifact_id));
      if (keyframes.length !== selected.length) throw new Error("部分草稿已更新，请重新选择关键帧。");
      const receipt = await saveSegment(workspace, {
        ...segmentInput(start, end, keyframes), source_video_id: detail.source_video_id,
        target_dataset_id: dataset.dataset_id, segment_id: id || undefined, expected_revision: expected,
      });
      setId(receipt.segment_id);
      setExpected(receipt.revision);
      setNotice("片段已保存。");
      onChanged();
    } catch (failure) {
      setError(workspaceCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  return <section aria-label="视频片段">
    <h3>片段</h3>
    <form onSubmit={event => { event.preventDefault(); void save(); }}>
      <fieldset disabled={disabled || busy}>
        <label>选择片段 <select value={id} onChange={event => edit(segments.find(segment => segment.segment_id === event.target.value))}>
          <option value="">新建片段</option>
          {segments.map(segment => <option key={segment.segment_id} value={segment.segment_id}>
            {segment.start_us}–{segment.end_us} µs · {segment.keyframes.length} 个关键帧
          </option>)}
        </select></label>
        <label>开始（微秒） <input inputMode="numeric" maxLength={19} value={start} onChange={event => setStart(event.target.value)} /></label>
        <label>结束（微秒，不含） <input inputMode="numeric" maxLength={19} value={end} onChange={event => setEnd(event.target.value)} /></label>
        <ul>{choices.map(({ frame_ref: frame, annotation_ref: draft }) => <li key={draft.artifact_id}>
          <label><input type="checkbox" checked={selected.includes(draft.artifact_id)}
            onChange={event => setSelected(event.target.checked
              ? [...selected, draft.artifact_id] : selected.filter(value => value !== draft.artifact_id))} />
            帧 {frame.frame_index} · {frame.pts_us ?? "时间未知"} µs · {draft.sha256.slice(0, 8)}
          </label>
        </li>)}</ul>
        <button type="submit">保存片段</button>
      </fieldset>
    </form>
    {notice && <p role="status">{notice}</p>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
