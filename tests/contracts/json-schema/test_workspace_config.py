"""Verify the on-disk Workspace marker and explicitly scoped defaults."""

import unittest

from schema_checks import REPO, SchemaVectorChecks


class WorkspaceConfigTests(SchemaVectorChecks, unittest.TestCase):
    schema_filename = "workspace-config.schema.json"
    case_file = REPO / "testdata/contracts/json-schema/v1/workspace-config/cases.json"

    def test_name_length_counts_unicode_characters(self):
        prefix = (
            '{"config_format_version":1,"config_revision":0,'
            '"workspace_id":"2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9","name":"'
        )
        for count, expected in (
            (1, {"stage": "ok", "category": "accepted"}),
            (128, {"stage": "ok", "category": "accepted"}),
            (129, {"stage": "schema", "category": "maxLength"}),
        ):
            with self.subTest(length=count):
                raw = f'{prefix}{"图" * count}"}}'.encode("utf-8")
                self.assert_outcome(f"name_length_{count}", raw, expected)


if __name__ == "__main__":
    unittest.main()
