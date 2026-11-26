import type { ColorMethod, ColorOptions, ColorPreviewView, ColorRegion } from "./api/generated/wire-primitives";

export type ColorForm = {
  shape: ColorRegion["kind"];
  bbox: [string, string, string, string];
  points: string;
  method: ColorMethod;
  target: string;
  strength: string;
  feather: string;
  hueGate: boolean;
  hueMin: string;
  hueMax: string;
};

type ImageSize = { width: number; height: number };
type ColorScope = Pick<ColorPreviewView, "asset_id" | "dataset_id" | "base_image_digest">;
type DisplayedPreview = {
  preview: ColorPreviewView;
  intent: string;
  renderedDigest: string | null;
};

function readNumber(input: string, message: string): number {
  const value = Number(input);
  if (!input.trim() || !Number.isFinite(value)) throw new Error(message);
  return value;
}

function readRegion(form: ColorForm, image: ImageSize): ColorRegion {
  const coordinate = (value: string) => readNumber(value, "请输入有效的原图像素坐标。");
  if (form.shape === "RECT") {
    const [x, y, width, height] = form.bbox.map(coordinate);
    if (x < 0 || y < 0 || width <= 0 || height <= 0 || x + width > image.width || y + height > image.height) {
      throw new Error("矩形区域须有正面积并位于图像内。");
    }
    return { kind: "RECT", bbox_xywh: [x, y, width, height] };
  }
  const points_xy = form.points.trim().split(/\r?\n/).map((line): [number, number] => {
    const pair = line.split(",");
    if (pair.length !== 2) throw new Error("请在每行按 x,y 输入一个顶点。");
    const [x, y] = pair.map(coordinate);
    if (x < 0 || y < 0 || x > image.width || y > image.height) throw new Error("多边形顶点须位于图像内。");
    return [x, y];
  });
  if (points_xy.length < 3 || new Set(points_xy.map(point => point.join(","))).size !== points_xy.length) {
    throw new Error("多边形须包含至少三个互不重复的顶点。");
  }
  return { kind: "POLYGON", points_xy };
}

export function colorIntent(form: ColorForm, image: ImageSize): {
  region: ColorRegion;
  operation: ColorOptions;
} {
  const region = readRegion(form, image);
  if (!/^#[0-9a-f]{6}$/i.test(form.target)) throw new Error("请选择六位 sRGB 颜色。");
  const channel = (offset: number) => Number.parseInt(form.target.slice(offset, offset + 2), 16);
  const strength = readNumber(form.strength, "请输入改色强度。");
  const feather_px = readNumber(form.feather, "请输入羽化像素数。");
  if (strength < 0 || strength > 1 || feather_px < 0 || feather_px > 64) {
    throw new Error("强度须在 0–1，羽化须在 0–64 像素。");
  }
  const operation: ColorOptions = {
    method: form.method, target_rgb: [channel(1), channel(3), channel(5)], strength, feather_px,
  };
  if (form.hueGate) {
    const hue_min_deg = readNumber(form.hueMin, "请输入原色相范围。");
    const hue_max_deg = readNumber(form.hueMax, "请输入原色相范围。");
    if (hue_min_deg < 0 || hue_min_deg >= 360 || hue_max_deg < 0 || hue_max_deg >= 360) {
      throw new Error("色相须小于 360° 且不小于 0°。");
    }
    operation.source_color_gate = { hue_min_deg, hue_max_deg, wraps_zero: hue_min_deg > hue_max_deg };
  }
  return { region, operation };
}

export function canApproveColor(displayed: DisplayedPreview | null, scope: ColorScope, currentIntent: string): boolean {
  if (!displayed || displayed.intent !== currentIntent) return false;
  const { preview, renderedDigest } = displayed;
  return preview.asset_id === scope.asset_id && preview.dataset_id === scope.dataset_id
    && preview.base_image_digest === scope.base_image_digest && preview.changed_pixels !== "0"
    && preview.candidate_ref.owner.kind === "TASK" && preview.candidate_ref.kind === "REPAIR_CANDIDATE"
    && renderedDigest === preview.candidate_ref.sha256;
}
