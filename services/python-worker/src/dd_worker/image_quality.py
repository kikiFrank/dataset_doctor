"""Measure exposure and local contrast in decoded image pixels."""

from dataclasses import dataclass
from math import isfinite

from PIL import Image, ImageChops


@dataclass(frozen=True, slots=True)
class ImageQualityPolicy:
    dark_luma_max: int
    bright_luma_min: int
    exposure_fraction: float
    min_gradient_energy: float

    def validate(self) -> None:
        if (
            type(self.dark_luma_max) is not int or type(self.bright_luma_min) is not int
            or not 0 <= self.dark_luma_max < self.bright_luma_min <= 255
            or not isfinite(self.exposure_fraction) or not 0 < self.exposure_fraction <= 1
            or not isfinite(self.min_gradient_energy) or self.min_gradient_energy < 0
        ):
            raise ValueError("invalid image quality thresholds")


@dataclass(frozen=True, slots=True)
class ImageQualityMeasurement:
    visible_pixels: int
    dark_fraction: float | None
    bright_fraction: float | None
    gradient_energy: float | None
    findings: tuple[str, ...]


def measure_image_quality(image: Image.Image, policy: ImageQualityPolicy) -> ImageQualityMeasurement:
    policy.validate()

    visible = None
    if "A" in image.getbands() or "transparency" in image.info:
        with image.convert("RGBA") as rgba:
            visible = rgba.getchannel("A").point([0] + [255] * 255)
    try:
        with image.convert("L") as luminance:
            distribution = luminance.histogram(mask=visible)
            count = sum(distribution)
            if count == 0:
                return ImageQualityMeasurement(0, None, None, None, ())
            dark = sum(distribution[:policy.dark_luma_max + 1]) / count
            bright = sum(distribution[policy.bright_luma_min:]) / count
            energy = _gradient_energy(luminance, visible)
    finally:
        if visible is not None:
            visible.close()

    findings = []
    if dark >= policy.exposure_fraction:
        findings.append("image.underexposed")
    if bright >= policy.exposure_fraction:
        findings.append("image.overexposed")
    if energy is not None and energy < policy.min_gradient_energy:
        findings.append("image.blur")
    return ImageQualityMeasurement(count, dark, bright, energy, tuple(findings))


def _gradient_energy(luminance: Image.Image, visible: Image.Image | None) -> float | None:
    width, height = luminance.size
    neighbors = []
    if width > 1:
        neighbors.append(((0, 0, width - 1, height), (1, 0, width, height)))
    if height > 1:
        neighbors.append(((0, 0, width, height - 1), (0, 1, width, height)))
    squared_sum = pair_count = 0
    for first, second in neighbors:
        with ImageChops.difference(luminance.crop(first), luminance.crop(second)) as difference:
            if visible is None:
                histogram = difference.histogram()
            else:
                with ImageChops.multiply(visible.crop(first), visible.crop(second)) as pairs:
                    histogram = difference.histogram(mask=pairs)
        pair_count += sum(histogram)
        squared_sum += sum(value * value * count for value, count in enumerate(histogram))
    return squared_sum / pair_count if pair_count else None
