"""Golden decoded pixels and encoded candidate bytes for color operations."""

from hashlib import sha256
from io import BytesIO
from pathlib import Path
import struct
import sys
import unittest
import zlib

from PIL import Image

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))


def encode(mode, size, pixels, format="PNG", **options):
    with BytesIO() as output, Image.new(mode, size) as image:
        image.putdata(pixels)
        image.save(output, format=format, **options)
        return output.getvalue()


def pixels(encoded):
    with Image.open(BytesIO(encoded)) as image:
        return image.size, image.mode, list(image.get_flattened_data())


def rgb16_png():
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 16, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(b"\0\x11\xff\x22\xff\x33\xff"))
        + chunk(b"IEND", b"")
    )


class ColorRepairTests(unittest.TestCase):
    def test_fractional_rectangle_changes_only_covered_centers_and_retains_alpha(self):
        from dd_worker.color_repair import ColorOperation, Rectangle, render_color_candidate

        original = encode("RGBA", (4, 1), [(100, 20, 0, alpha) for alpha in (0, 25, 128, 255)])
        result = render_color_candidate(
            original, sha256(original).hexdigest(), Rectangle(0.75, 0, 2, 1),
            ColorOperation("SOLID_FILL", (0, 0, 200), strength=0.5), max_pixels=4,
        )
        self.assertEqual(pixels(result.encoded),
                         ((4, 1), "RGBA", [(100, 20, 0, 0), (50, 10, 100, 25),
                                           (50, 10, 100, 128), (100, 20, 0, 255)]))
        self.assertEqual(pixels(result.mask_png), ((4, 1), "L", [0, 128, 128, 0]))
        self.assertEqual(result.sha256, sha256(result.encoded).hexdigest())
        self.assertEqual(result.changed_pixels, 2)
        self.assertFalse(result.lossy_reencode)

    def test_triangle_mask_uses_pixel_centers_and_feathers_only_inside(self):
        from dd_worker.color_repair import ColorOperation, Polygon, render_color_candidate

        original = encode("RGB", (4, 4), [(0, 0, 0)] * 16)
        result = render_color_candidate(
            original, sha256(original).hexdigest(), Polygon(((0, 0), (3, 0), (0, 3))),
            ColorOperation("SOLID_FILL", (240, 120, 0), feather_px=1), max_pixels=16,
        )
        self.assertEqual(pixels(result.mask_png)[2], [
            128, 128, 0, 0,
            128, 0, 0, 0,
            0, 0, 0, 0,
            0, 0, 0, 0,
        ])
        self.assertEqual(pixels(result.encoded)[2][1], (120, 60, 0))
        self.assertEqual(pixels(result.encoded)[2][8], (0, 0, 0))

    def test_wrapped_source_hues_preserve_saturation_value_and_unmatched_pixels(self):
        from dd_worker.color_repair import ColorOperation, HueGate, Rectangle, render_color_candidate

        original = encode("RGB", (4, 1), [(120, 60, 60), (120, 60, 70),
                                          (60, 120, 60), (110, 110, 110)])
        result = render_color_candidate(
            original, sha256(original).hexdigest(), Rectangle(0, 0, 4, 1),
            ColorOperation("HUE_REMAP", (0, 0, 255), gate=HueGate(340, 15, True)),
            max_pixels=4,
        )
        self.assertEqual(pixels(result.encoded)[2], [(60, 60, 120), (60, 60, 120),
                                                       (60, 120, 60), (110, 110, 110)])
        self.assertEqual(pixels(result.mask_png)[2], [255, 255, 0, 0])

    def test_oriented_jpeg_is_rendered_once_and_zero_strength_preserves_bytes(self):
        from dd_worker.color_repair import ColorOperation, Rectangle, render_color_candidate

        exif = Image.Exif()
        exif[274] = 6
        original = encode("RGB", (32, 16), [(255, 0, 0)] * 512, "JPEG", exif=exif)
        expected = sha256(original).hexdigest()
        region = Rectangle(0, 0, 16, 32)
        noop = render_color_candidate(
            original, expected, region, ColorOperation("SOLID_FILL", (0, 255, 0), strength=0),
            max_pixels=512,
        )
        self.assertEqual(noop.encoded, original)
        self.assertEqual(noop.changed_pixels, 0)
        result = render_color_candidate(
            original, expected, region, ColorOperation("SOLID_FILL", (0, 255, 0)),
            max_pixels=512,
        )
        self.assertTrue(result.lossy_reencode)
        with Image.open(BytesIO(result.encoded)) as image:
            self.assertEqual(image.format, "JPEG")
            self.assertEqual(image.size, (16, 32))
            self.assertEqual(image.getexif().get(274, 1), 1)
            red, green, blue = image.getpixel((8, 16))
            self.assertTrue(green > 245 and max(red, blue) < 10)

    def test_rejects_invalid_geometry_gate_source_and_budget(self):
        from dd_worker.color_repair import (
            ColorOperation, ColorRepairError, HueGate, Polygon, Rectangle, render_color_candidate,
        )

        original = encode("RGB", (4, 4), [(255, 0, 0)] * 16)
        valid = ColorOperation("HUE_REMAP", (0, 255, 0))
        rectangle = Rectangle(0, 0, 4, 4)
        digest = sha256(original).hexdigest()
        vectors = [
            ("source_changed", original, "0" * 64, rectangle, valid, 16),
            ("pixel_limit_exceeded", original, digest, rectangle, valid, 15),
            ("invalid_region", original, digest, Rectangle(1, 1, 4, 3), valid, 16),
            ("invalid_region", original, digest, Rectangle(float("nan"), 0, 1, 1), valid, 16),
            ("invalid_region", original, digest, Polygon(((0, 0), (3, 3), (0, 3), (3, 0))), valid, 16),
            ("invalid_region", original, digest, Polygon(((0, 0), (2, 1), (1, 0.5))), valid, 16),
            ("invalid_operation", original, digest, rectangle,
             ColorOperation("HUE_REMAP", (0, 256, 0)), 16),
            ("invalid_operation", original, digest, rectangle,
             ColorOperation("HUE_REMAP", (0, 255, 0), gate=HueGate(40, 20, False)), 16),
        ]
        for unsupported in (encode("L", (1, 1), [10]), rgb16_png()):
            vectors.append(("unsupported_color_input", unsupported, sha256(unsupported).hexdigest(),
                            Rectangle(0, 0, 1, 1), valid, 16))
        for code, encoded, expected, region, operation, limit in vectors:
            with self.subTest(code=code, region=region, operation=operation):
                with self.assertRaises(ColorRepairError) as error:
                    render_color_candidate(encoded, expected, region, operation, max_pixels=limit)
                self.assertEqual(error.exception.code, code)


if __name__ == "__main__":
    unittest.main(testRunner=unittest.TextTestRunner(stream=sys.stdout, verbosity=2))
