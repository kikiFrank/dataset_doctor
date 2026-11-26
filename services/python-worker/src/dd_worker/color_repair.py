"""Render an immutable color candidate in orientation-normalized sRGB pixels."""

from colorsys import hsv_to_rgb, rgb_to_hsv
from dataclasses import dataclass
from hashlib import sha256
from io import BytesIO
from math import ceil, floor, hypot, isfinite

from PIL import Image, ImageOps

type Point = tuple[float, float]
type Edge = tuple[Point, Point]
type Bounds = tuple[float, float, float, float]


class ColorRepairError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


@dataclass(frozen=True, slots=True)
class Rectangle:
    x: float
    y: float
    width: float
    height: float


@dataclass(frozen=True, slots=True)
class Polygon:
    points: tuple[Point, ...]


@dataclass(frozen=True, slots=True)
class HueGate:
    minimum: float
    maximum: float
    wraps_zero: bool

    def matches(self, hue: float) -> bool:
        degrees = hue * 360
        if self.wraps_zero:
            return degrees >= self.minimum or degrees <= self.maximum
        return self.minimum <= degrees <= self.maximum


@dataclass(frozen=True, slots=True)
class ColorOperation:
    method: str
    target_rgb: tuple[int, int, int]
    strength: float = 1
    feather_px: float = 0
    gate: HueGate | None = None

    def validate(self) -> None:
        if (
            self.method not in ("SOLID_FILL", "HUE_REMAP")
            or len(self.target_rgb) != 3
            or any(type(color) is not int or not 0 <= color <= 255 for color in self.target_rgb)
            or not 0 <= self.strength <= 1
            or not 0 <= self.feather_px <= 64
        ):
            raise ColorRepairError("invalid_operation")
        if self.gate is not None:
            gate = self.gate
            if (
                not 0 <= gate.minimum < 360
                or not 0 <= gate.maximum < 360
                or type(gate.wraps_zero) is not bool
                or gate.wraps_zero != (gate.minimum > gate.maximum)
            ):
                raise ColorRepairError("invalid_operation")


@dataclass(frozen=True, slots=True)
class ColorCandidate:
    encoded: bytes
    sha256: str
    mask_png: bytes
    changed_pixels: int
    lossy_reencode: bool


def render_color_candidate(
    original: bytes,
    expected_sha256: str,
    region: Rectangle | Polygon,
    operation: ColorOperation,
    *,
    max_pixels: int,
) -> ColorCandidate:
    if sha256(original).hexdigest() != expected_sha256:
        raise ColorRepairError("source_changed")
    operation.validate()
    try:
        with Image.open(BytesIO(original)) as source:
            image_format = source.format
            if source.width * source.height > max_pixels:
                raise ColorRepairError("pixel_limit_exceeded")
            _validate_color_input(source, original)
            source.load()
            with ImageOps.exif_transpose(source) as oriented:
                mode = "RGBA" if "transparency" in oriented.info else oriented.mode
                with oriented.convert(mode) as image, Image.new("L", oriented.size) as mask:
                    bounds, edges = _region(region, image.size)
                    changed = _paint(image, mask, bounds, edges, operation)
                    encoded = original
                    if changed:
                        if image_format == "JPEG":
                            encoded = _encode(image, image_format, quality=95, subsampling=0)
                        else:
                            encoded = _encode(image, image_format)
                    return ColorCandidate(
                        encoded, sha256(encoded).hexdigest(), _encode(mask, "PNG"), changed,
                        bool(changed and image_format == "JPEG"),
                    )
    except ColorRepairError:
        raise
    except Image.DecompressionBombError:
        raise ColorRepairError("pixel_limit_exceeded") from None
    except (OSError, SyntaxError):
        raise ColorRepairError("decode_failed") from None


def _validate_color_input(source: Image.Image, original: bytes) -> None:
    if (
        source.format not in ("PNG", "JPEG")
        or source.mode not in ("RGB", "RGBA")
        or source.info.get("icc_profile")
        or getattr(source, "n_frames", 1) != 1
    ):
        raise ColorRepairError("unsupported_color_input")
    if source.format == "PNG":
        gamma = source.info.get("gamma")
        if original[24] != 8 or ("srgb" not in source.info and (
            "chromaticity" in source.info or (gamma is not None and abs(gamma - 0.45455) > 0.0001)
        )):
            raise ColorRepairError("unsupported_color_input")


def _region(region: Rectangle | Polygon, size: tuple[int, int]) -> tuple[Bounds, tuple[Edge, ...]]:
    width, height = size
    if isinstance(region, Rectangle):
        right = region.x + region.width
        bottom = region.y + region.height
        if not (0 <= region.x < right <= width and 0 <= region.y < bottom <= height):
            raise ColorRepairError("invalid_region")
        return (region.x, region.y, right, bottom), ()
    if not isinstance(region, Polygon):
        raise ColorRepairError("invalid_region")
    points = region.points
    if len(points) < 3 or len(set(points)) != len(points) or any(
        not (isfinite(x) and isfinite(y) and 0 <= x <= width and 0 <= y <= height)
        for x, y in points
    ):
        raise ColorRepairError("invalid_region")
    edges = tuple(zip(points, points[1:] + points[:1]))
    for index, (a, b) in enumerate(edges):
        c = points[(index + 2) % len(points)]
        if _cross(a, b, c) == 0 and (_on_segment(a, b, c) or _on_segment(b, c, a)):
            raise ColorRepairError("invalid_region")
        for other in range(index + 2, len(edges) - (index == 0)):
            if _intersect(a, b, *edges[other]):
                raise ColorRepairError("invalid_region")
    xs, ys = zip(*points)
    return (min(xs), min(ys), max(xs), max(ys)), edges


def _cross(a: Point, b: Point, p: Point) -> float:
    return (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])


def _on_segment(a: Point, b: Point, p: Point) -> bool:
    return (min(a[0], b[0]) <= p[0] <= max(a[0], b[0])
            and min(a[1], b[1]) <= p[1] <= max(a[1], b[1]))


def _intersect(a: Point, b: Point, c: Point, d: Point) -> bool:
    ab_c, ab_d = _cross(a, b, c), _cross(a, b, d)
    cd_a, cd_b = _cross(c, d, a), _cross(c, d, b)
    return (
        (ab_c * ab_d < 0 and cd_a * cd_b < 0)
        or (ab_c == 0 and _on_segment(a, b, c))
        or (ab_d == 0 and _on_segment(a, b, d))
        or (cd_a == 0 and _on_segment(c, d, a))
        or (cd_b == 0 and _on_segment(c, d, b))
    )


def _contains(edges: tuple[Edge, ...], x: float, y: float) -> bool:
    inside = False
    for a, b in edges:
        if _cross(a, b, (x, y)) == 0 and _on_segment(a, b, (x, y)):
            return True
        if (a[1] > y) != (b[1] > y):
            crossing = a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1])
            if x < crossing:
                inside = not inside
    return inside


def _distance(a: Point, b: Point, x: float, y: float) -> float:
    dx, dy = b[0] - a[0], b[1] - a[1]
    length = hypot(dx, dy)
    dx, dy = dx / length, dy / length
    projection = max(0.0, min(length, (x - a[0]) * dx + (y - a[1]) * dy))
    return hypot(x - a[0] - projection * dx, y - a[1] - projection * dy)


def _paint(
    image: Image.Image,
    mask: Image.Image,
    bounds: Bounds,
    edges: tuple[Edge, ...],
    operation: ColorOperation,
) -> int:
    left, top, right, bottom = bounds
    output = image.load()
    weights = mask.load()
    changed = 0
    if operation.strength == 0:
        return changed
    remap = operation.method == "HUE_REMAP"
    target_hue, _, _ = rgb_to_hsv(*(component / 255 for component in operation.target_rgb))
    for row in range(floor(top), ceil(bottom)):
        y = row + 0.5
        for col in range(floor(left), ceil(right)):
            x = col + 0.5
            if edges:
                inside = _contains(edges, x, y)
            else:
                inside = left <= x < right and top <= y < bottom
            if not inside:
                continue
            pixel = output[col, row]
            if remap or operation.gate is not None:
                hue, saturation, value = rgb_to_hsv(*(component / 255 for component in pixel[:3]))
                if operation.gate is not None and (saturation == 0 or not operation.gate.matches(hue)):
                    continue
            alpha = operation.strength
            if operation.feather_px:
                distance = (min(_distance(a, b, x, y) for a, b in edges) if edges
                            else min(x - left, y - top, right - x, bottom - y))
                alpha *= min(1.0, distance / operation.feather_px)
            target = operation.target_rgb
            if remap:
                target = tuple(channel * 255 for channel in hsv_to_rgb(target_hue, saturation, value))
            rgb = tuple(floor(channel + (selected - channel) * alpha + 0.5)
                        for channel, selected in zip(pixel[:3], target))
            weights[col, row] = floor(alpha * 255 + 0.5)
            if rgb != pixel[:3]:
                output[col, row] = rgb + pixel[3:]
                changed += 1
    return changed


def _encode(image: Image.Image, image_format: str, **options) -> bytes:
    with BytesIO() as output:
        image.save(output, format=image_format, **options)
        return output.getvalue()
