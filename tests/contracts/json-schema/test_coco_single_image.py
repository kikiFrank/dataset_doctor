"""Verify the single-image COCO bbox file contract."""

import unittest

from schema_checks import REPO, SchemaVectorChecks


class CocoSingleImageTests(SchemaVectorChecks, unittest.TestCase):
    schema_filename = "coco-single-image.schema.json"
    case_file = REPO / "testdata/contracts/json-schema/v1/coco-single-image/cases.json"


if __name__ == "__main__":
    unittest.main()
