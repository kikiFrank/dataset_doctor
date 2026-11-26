"""Deterministic geometry checks on a synthetic single-image annotation."""

import json
from pathlib import Path
import sys
import unittest

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))
from dd_worker.annotation_geometry import audit_geometry

FIXTURES = REPO / "testdata/contracts/json-schema/v1/coco-single-image/cases.json"


def document():
    vectors = json.loads(FIXTURES.read_text(encoding="utf-8"))
    baseline = next(vector for vector in vectors if vector["id"] == "documented_profile")
    return json.loads(baseline["raw"])


class AnnotationGeometryTests(unittest.TestCase):
    def test_valid_geometry_checks_against_decoded_dimensions(self):
        result = audit_geometry(document(), width=1920, height=1080)
        self.assertEqual(result.status, "COMPLETED")
        self.assertEqual(result.findings, ())

    def test_distinguishes_missing_json_from_empty_annotations(self):
        missing = audit_geometry(None, width=1920, height=1080)
        self.assertEqual(missing.status, "NOT_EXECUTED")
        self.assertEqual([item.code for item in missing.findings], ["annotation_missing"])
        current = document()
        current["annotations"] = []
        empty = audit_geometry(current, width=1920, height=1080)
        self.assertEqual(empty.status, "COMPLETED")
        self.assertEqual([item.code for item in empty.findings], ["annotation_empty"])

    def test_reports_reference_errors_duplicates_and_bad_bounds_without_mutation(self):
        current = document()
        current["images"][0]["width"] = 100
        first = current["annotations"][0]
        first.update(image_id=8, category_id=999, bbox=[-1, 80, 40, 60])
        current["annotations"].append(dict(first, image_id=1, category_id=3, bbox=[3, 8, 0, 9]))
        before = json.dumps(current)
        result = audit_geometry(current, width=1920, height=1080)
        self.assertEqual(result.status, "COMPLETED")
        self.assertEqual(
            [(item.code, item.annotation_id) for item in result.findings],
            [
                ("dimension_mismatch", None),
                ("image_reference_mismatch", 1),
                ("unknown_category", 1),
                ("bbox_out_of_bounds", 1),
                ("duplicate_annotation_id", 1),
                ("invalid_bbox", 1),
            ],
        )
        self.assertEqual(json.dumps(current), before)

    def test_rejects_nonfinite_bbox_from_a_projection(self):
        current = document()
        current["annotations"][0]["bbox"] = [0, 0, float("inf"), 1]
        result = audit_geometry(current, width=1920, height=1080)
        self.assertEqual([item.code for item in result.findings], ["invalid_bbox"])


if __name__ == "__main__":
    unittest.main()
