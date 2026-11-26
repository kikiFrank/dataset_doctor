"""Image quality rules against decoded synthetic PNG pixels."""

from io import BytesIO
from pathlib import Path
import sys
import unittest

from PIL import Image

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))
from dd_worker.image_quality import ImageQualityPolicy, measure_image_quality


POLICY = ImageQualityPolicy(
    dark_luma_max=20, bright_luma_min=235, exposure_fraction=0.9, min_gradient_energy=100,
)


def measure(mode, size, pixels, policy=POLICY):
    buffer = BytesIO()
    with Image.new(mode, size) as image:
        image.putdata(pixels)
        image.save(buffer, "PNG")
    buffer.seek(0)
    with Image.open(buffer) as image:
        image.load()
        return measure_image_quality(image, policy)


class ImageQualityTests(unittest.TestCase):
    def test_dark_bright_and_low_texture_findings_are_independent(self):
        dark = measure("L", (4, 4), [20] * 16)
        bright = measure("L", (4, 4), [235] * 16)
        mid = measure("L", (4, 4), [110] * 16)
        self.assertEqual((dark.dark_fraction, dark.bright_fraction, dark.gradient_energy), (1, 0, 0))
        self.assertEqual(dark.findings, ("image.underexposed", "image.blur"))
        self.assertEqual((bright.dark_fraction, bright.bright_fraction, bright.gradient_energy), (0, 1, 0))
        self.assertEqual(bright.findings, ("image.overexposed", "image.blur"))
        self.assertEqual(mid.findings, ("image.blur",))

    def test_checkerboard_retains_its_edge_energy_without_exposure_findings(self):
        result = measure("L", (3, 3), [0, 255, 0, 255, 0, 255, 0, 255, 0])
        self.assertEqual(result.visible_pixels, 9)
        self.assertAlmostEqual(result.dark_fraction, 5 / 9)
        self.assertAlmostEqual(result.bright_fraction, 4 / 9)
        self.assertEqual(result.gradient_energy, 255 ** 2)
        self.assertEqual(result.findings, ())

    def test_uses_visible_pixels_and_adjacent_pairs_only(self):
        isolated = measure("RGBA", (2, 2), [
            (0, 0, 0, 255), (255, 255, 255, 0), (255, 255, 255, 0), (255, 255, 255, 0),
        ])
        self.assertEqual(isolated.visible_pixels, 1)
        self.assertEqual(isolated.dark_fraction, 1)
        self.assertIsNone(isolated.gradient_energy)
        self.assertEqual(isolated.findings, ("image.underexposed",))
        transparent = measure("RGBA", (1, 1), [(255, 255, 255, 0)])
        self.assertEqual(transparent.visible_pixels, 0)
        self.assertIsNone(transparent.dark_fraction)
        self.assertIsNone(transparent.bright_fraction)
        self.assertIsNone(transparent.gradient_energy)
        self.assertEqual(transparent.findings, ())

    def test_rejects_overlapping_nonfinite_or_out_of_range_thresholds(self):
        failures = [
            ImageQualityPolicy(235, 20, 0.9, 100),
            ImageQualityPolicy(20, 235, float("nan"), 100),
            ImageQualityPolicy(20, 235, 0, 100),
            ImageQualityPolicy(20, 235, 1.01, 100),
            ImageQualityPolicy(20, 235, 0.9, float("inf")),
            ImageQualityPolicy(20, 235, 0.9, -1),
        ]
        for policy in failures:
            with self.subTest(policy=policy), self.assertRaises(ValueError):
                measure("L", (1, 1), [100], policy)


if __name__ == "__main__":
    unittest.main()
