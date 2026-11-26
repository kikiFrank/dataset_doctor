"""Offline profile counts from actual decoded images and per-image COCO documents."""

from io import BytesIO
from pathlib import Path
import sys
import unittest

from PIL import Image

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))
from dd_worker.dataset_profile import ProfileSample, profile_dataset

PNG = (REPO / "testdata/image_io/p00-worker-rgb.png").read_bytes()


def rotated_jpeg():
    output = BytesIO()
    with Image.new("RGB", (3, 2), "#00855a") as image:
        exif = Image.Exif()
        exif[274] = 6
        image.save(output, format="JPEG", exif=exif)
    return output.getvalue()


def coco(category, name, references):
    return {
        "images": [{"id": 1, "file_name": "synthetic.png", "width": 2, "height": 1}],
        "categories": [{"id": category, "name": name}],
        "annotations": [
            {"id": index, "image_id": 1, "category_id": ref, "bbox": [0, 0, 1, 1],
             "area": 1, "iscrowd": 0}
            for index, ref in enumerate(references, 1)
        ],
    }


class DatasetProfileTests(unittest.TestCase):
    def test_counts_categories_by_name_and_actual_exif_corrected_resolutions(self):
        samples = [
            ProfileSample("a", PNG, coco(4, "green-light", [4, 4])),
            ProfileSample("b", rotated_jpeg(), coco(26, "green-light", [26])),
            ProfileSample("c", PNG, coco(9, "red-light", [])),
            ProfileSample("d", PNG, None),
        ]
        result = profile_dataset(samples, max_pixels=6)
        self.assertEqual(result.asset_count, 4)
        self.assertEqual(result.annotation_count, 3)
        self.assertEqual(result.resolution_histogram, {(2, 1): 3, (2, 3): 1})
        self.assertEqual(result.class_counts, {"green-light": 3})
        self.assertEqual(result.empty_annotation_count, 1)
        self.assertEqual(result.unannotated_count, 1)
        self.assertEqual(result.file_errors, ())

    def test_corrupt_sources_and_unknown_category_references_do_not_poison_other_counts(self):
        result = profile_dataset([
            ProfileSample("corrupt", b"bad image", coco(4, "green-light", [4])),
            ProfileSample("unknown", PNG, coco(4, "green-light", [4, 5])),
            ProfileSample("budget", rotated_jpeg(), None),
            ProfileSample("valid", PNG, coco(7, "red-light", [7])),
        ], max_pixels=2)
        self.assertEqual(result.asset_count, 4)
        self.assertEqual(result.resolution_histogram, {(2, 1): 2})
        self.assertEqual(result.annotation_count, 2)
        self.assertEqual(result.class_counts, {"green-light": 1, "red-light": 1})
        self.assertEqual(result.empty_annotation_count, 0)
        self.assertEqual(result.unannotated_count, 1)
        self.assertEqual(
            [(error.asset_id, error.source, error.code) for error in result.file_errors],
            [("corrupt", "image", "decode_failed"), ("unknown", "annotation", "unknown_category"),
             ("budget", "image", "pixel_limit_exceeded")],
        )

    def test_empty_selection_and_invalid_pixel_limit(self):
        result = profile_dataset(iter(()), max_pixels=1)
        self.assertEqual(result.asset_count, 0)
        self.assertEqual(result.annotation_count, 0)
        self.assertEqual(result.resolution_histogram, {})
        with self.assertRaisesRegex(ValueError, "max_pixels"):
            profile_dataset([], max_pixels=0)


if __name__ == "__main__":
    unittest.main()
