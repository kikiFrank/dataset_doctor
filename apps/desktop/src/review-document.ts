import type { CocoBox, CocoCategory, CocoDocument, CocoImage } from "./api/generated/wire-primitives";

export type BoxCoordinates = CocoBox["bbox"];
export type ImagePoint = [number, number];
export type EditableBox = Pick<CocoBox, "id" | "bbox" | "iscrowd"> & { category_id: number | null };

export function seedBoxes(source: CocoDocument | null, categories: CocoCategory[]): EditableBox[] {
  if (!source) return [];
  const sourceCategories = new Map<number, string | null>();
  for (const category of source.categories) {
    sourceCategories.set(category.id, sourceCategories.has(category.id) ? null : category.name);
  }
  const registered = new Map(categories.map(({ name, id }) => [name, id]));
  return source.annotations.map((box) => {
    const name = sourceCategories.get(box.category_id);
    return {
      id: box.id,
      category_id: box.image_id === source.images[0]?.id && name ? registered.get(name) ?? null : null,
      bbox: [...box.bbox],
      iscrowd: box.iscrowd,
    };
  });
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
    return { ...box, image_id: image.id, category_id: category, area: width * height };
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
