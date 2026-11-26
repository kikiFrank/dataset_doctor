"""Real PNG and JPEG decoding at the Worker adapter boundary."""

from hashlib import sha256
from io import BytesIO
from pathlib import Path
import sys
import unittest

from PIL import Image

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))
from dd_worker.image_inspection import ImageInspectionError, inspect_image

FIXTURE = REPO / "testdata/image_io/p00-worker-rgb.png"


class ImageInspectionTests(unittest.TestCase):
    def test_reads_complete_png_and_uses_original_byte_digest(self):
        encoded = FIXTURE.read_bytes()
        result = inspect_image(encoded, max_pixels=2)
        self.assertEqual((result.width, result.height, result.media_type), (2, 1, "image/png"))
        self.assertEqual(result.sha256, sha256(encoded).hexdigest())

    def test_jpeg_exif_orientation_returns_display_dimensions(self):
        buffer = BytesIO()
        with Image.new("RGB", (3, 2), "#187e73") as image:
            exif = Image.Exif()
            exif[274] = 6
            image.save(buffer, format="JPEG", exif=exif)
        encoded = buffer.getvalue()
        result = inspect_image(encoded, max_pixels=6)
        self.assertEqual((result.width, result.height, result.media_type), (2, 3, "image/jpeg"))
        self.assertEqual(result.sha256, sha256(encoded).hexdigest())

    def test_rejects_truncated_or_unknown_bytes(self):
        encoded = FIXTURE.read_bytes()[:40]
        for fixture in [encoded, b"not an image"]:
            with self.subTest(size=len(fixture)):
                with self.assertRaises(ImageInspectionError) as error:
                    inspect_image(fixture, max_pixels=100)
                self.assertEqual(error.exception.code, "decode_failed")

    def test_rejects_image_over_budget_and_unsupported_formats(self):
        with self.assertRaises(ImageInspectionError) as error:
            inspect_image(FIXTURE.read_bytes(), max_pixels=1)
        self.assertEqual(error.exception.code, "pixel_limit_exceeded")

        buffer = BytesIO()
        with Image.new("RGB", (1, 1)) as image:
            image.save(buffer, format="BMP")
        with self.assertRaises(ImageInspectionError) as error:
            inspect_image(buffer.getvalue(), max_pixels=1)
        self.assertEqual(error.exception.code, "unsupported_format")


if __name__ == "__main__":
    unittest.main()
