import type {
  CocoBox, CocoCategory, CocoDocument, CocoImage, ReviewTrackSelection, TrackObservationView,
} from "./api/generated/wire-primitives";

export type BoxCoordinates = CocoBox["bbox"];
export type ImagePoint = [number, number];
export type EditableBox = Pick<CocoBox, "id" | "bbox" | "iscrowd"> & {
  category_id: number | null;
  track_id?: string;
};

export function seedBoxes(
  source: CocoDocument | null, categories: CocoCategory[], bindings: ReviewTrackSelection[] = [],
): EditableBox[] {
  if (!source) return [];
  const sourceCategories = new Map<number, string | null>();
  for (const category of source.categories) {
    sourceCategories.set(category.id, sourceCategories.has(category.id) ? null : category.name);
  }
  const registered = new Map(categories.map(({ name, id }) => [name, id]));
  const tracks = new Map(bindings.map(({ local_annotation_id, track_id }) => [local_annotation_id, track_id]));
  return source.annotations.map((box) => {
    const name = sourceCategories.get(box.category_id);
    const category_id = box.image_id === source.images[0]?.id && name ? registered.get(name) ?? null : null;
    const track_id = tracks.get(box.id);
    return {
      id: box.id,
      category_id,
      bbox: [...box.bbox],
      iscrowd: box.iscrowd,
      ...(track_id !== undefined && box.category_id === category_id ? { track_id } : {}),
    };
  });
}

export function nextBoxId(boxes: EditableBox[]): number {
  const used = new Set(boxes.map(({ id }) => id));
  let id = 1;
  while (used.has(id)) id += 1;
  return id;
}

export function adoptTrackingBox(boxes: EditableBox[], candidate: TrackObservationView, categories: CocoCategory[]) {
  const category = categories.find(item => String(item.id) === candidate.category_id);
  if (!category || candidate.source !== "TRACKER" || boxes.some(box => box.track_id === candidate.track_id)) {
    return boxes;
  }
  const box: EditableBox = {
    id: nextBoxId(boxes), bbox: [...candidate.bbox_xywh],
    category_id: category.id, iscrowd: 0, track_id: candidate.track_id,
  };
  return [...boxes, box];
}

export function trackSelections(boxes: EditableBox[]): ReviewTrackSelection[] {
  return boxes.flatMap(({ id, track_id }) => track_id ? [{ local_annotation_id: id, track_id }] : []);
}

export function makeDraft(image: CocoImage, categories: CocoCategory[], boxes: EditableBox[]): CocoDocument {
  const registered = new Set(categories.map(({ id }) => id));
  const usedIds = new Set<number>();
  const annotations = boxes.map((box, index) => {
    const category = box.category_id;
    if (category === null || !registered.has(category)) throw new Error(`请选择第 ${index + 1} 个框的类别。`);
    if (usedIds.has(box.id)) throw new Error(`第 ${index + 1} 个框的编号重复，请删除重复框后重画。`);
    usedIds.add(box.id);
    const [x, y, width, height] = box.bbox;
    if (!box.bbox.every(Number.isFinite) || x < 0 || y < 0 || width <= 0 || height <= 0
        || width > image.width - x || height > image.height - y) {
      throw new Error(`第 ${index + 1} 个框超出图像范围或宽高无效。`);
    }
    return { id: box.id, bbox: box.bbox, iscrowd: box.iscrowd,
      image_id: image.id, category_id: category, area: width * height };
  });
  return { images: [image], annotations, categories };
}

export function boxFromDrag(
  start: ImagePoint, end: ImagePoint, image: Pick<CocoImage, "width" | "height">,
): BoxCoordinates | null {
  const clamp = (value: number, max: number) => Math.min(max, Math.max(0, value));
  const left = clamp(Math.min(start[0], end[0]), image.width);
  const top = clamp(Math.min(start[1], end[1]), image.height);
  const width = clamp(Math.max(start[0], end[0]), image.width) - left;
  const height = clamp(Math.max(start[1], end[1]), image.height) - top;
  return width > 0 && height > 0 ? [left, top, width, height] : null;
}
