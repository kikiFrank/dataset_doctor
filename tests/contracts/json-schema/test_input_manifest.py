"""Verify the InputManifest contract and strict JSON parsing boundaries."""

import sys
import unittest

from referencing.exceptions import Unresolvable

from schema_checks import DRAFT, REPO, SCHEMA_BASE, SchemaVectorChecks, strict_json


class InputManifestTests(SchemaVectorChecks, unittest.TestCase):
    schema_filename = "input-manifest.schema.json"
    case_file = REPO / "testdata/contracts/json-schema/v1/input-manifest/cases.json"

    def test_unknown_reference_is_rejected(self):
        unknown = f"{SCHEMA_BASE}missing.schema.json"
        with self.assertRaises(Unresolvable):
            self.registry.resolver(base_uri=self.schema_id).lookup(unknown)

        validator = self.validator.evolve(schema={"$schema": DRAFT, "$ref": unknown})
        with self.assertRaises(Unresolvable):
            validator.validate({})

    def test_overlong_integer_is_rejected_before_schema_validation(self):
        limit = sys.get_int_max_str_digits()
        self.assertGreater(limit, 0, "Python integer conversion protection is disabled")
        digits = b"9" * limit
        self.assertIsInstance(strict_json(digits), int)
        self.assert_outcome(
            "synthetic_overlong_integer",
            b'{"resolved_config":{"limit":' + digits + b"9}}",
            {"stage": "parse", "category": "integer_too_long"},
        )

    def test_non_utf8_is_rejected_before_schema_validation(self):
        self.assert_outcome(
            "synthetic_invalid_utf8",
            b'{"resolved_config":{"label":"\xff"}}',
            {"stage": "parse", "category": "invalid_utf8"},
        )


if __name__ == "__main__":
    unittest.main()
