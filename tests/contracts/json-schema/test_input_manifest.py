"""Validate the shared InputManifest vectors against the local Draft 2020-12 Schema."""

import json
import math
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator
from jsonschema.exceptions import SchemaError
from referencing import Registry, Resource
from referencing.exceptions import NoSuchResource, Unresolvable


REPO = Path(__file__).resolve().parents[3]
SCHEMA_DIR = REPO / "contracts/json-schema/v1"
CASE_FILE = REPO / "testdata/contracts/json-schema/v1/input-manifest/cases.json"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
SCHEMA_BASE = "https://datasetdoctor.invalid/contracts/json-schema/v1/"
SCHEMA_FILES = ("common.schema.json", "input-manifest.schema.json")
SCHEMA_IDS = {
    "datasetdoctor.input-manifest.v1": f"{SCHEMA_BASE}input-manifest.schema.json",
}


class JsonParseError(ValueError):
    """Report the parsing category without including the input."""

    def __init__(self, category):
        self.category = category
        super().__init__(category)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise JsonParseError("duplicate_key")
        result[key] = value
    return result


def reject_constant(_token):
    raise JsonParseError("non_finite_number")


def finite_float(token):
    value = float(token)
    if not math.isfinite(value):
        raise JsonParseError("non_finite_number")
    return value


def strict_json(data):
    try:
        text = data.decode("utf-8", errors="strict")
        return json.loads(
            text,
            object_pairs_hook=unique_object,
            parse_constant=reject_constant,
            parse_float=finite_float,
        )
    except UnicodeDecodeError:
        raise JsonParseError("invalid_utf8") from None
    except json.JSONDecodeError:
        raise JsonParseError("invalid_json") from None


def reject_external_reference(uri):
    raise NoSuchResource(ref=uri)


def references(node):
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "$ref":
                yield value
            else:
                yield from references(value)
    elif isinstance(node, list):
        for value in node:
            yield from references(value)


def schema_location(path):
    parts = (str(part).replace("~", "~0").replace("/", "~1") for part in path)
    return "#" + "".join(f"/{part}" for part in parts)


class InputManifestTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.schemas = {}
        for name in SCHEMA_FILES:
            path = SCHEMA_DIR / name
            try:
                schema = strict_json(path.read_bytes())
            except JsonParseError as error:
                raise AssertionError(f"{path}: parse/{error.category}") from None
            expected_id = f"{SCHEMA_BASE}{name}"
            if schema.get("$schema") != DRAFT or schema.get("$id") != expected_id:
                raise AssertionError(f"{path}: unexpected dialect or Schema ID")
            if expected_id in cls.schemas:
                raise AssertionError(f"{path}: duplicate Schema ID")
            try:
                Draft202012Validator.check_schema(schema)
            except SchemaError as error:
                location = schema_location(error.absolute_path)
                raise AssertionError(
                    f"{path}{location}: schema/{error.validator}"
                ) from None
            cls.schemas[expected_id] = schema

        cls.registry = Registry(retrieve=reject_external_reference).with_resources(
            (uri, Resource.from_contents(schema))
            for uri, schema in cls.schemas.items()
        )
        profile = "datasetdoctor.input-manifest.v1"
        cls.schema_id = SCHEMA_IDS[profile]
        cls.schema_path = SCHEMA_DIR / "input-manifest.schema.json"
        cls.validator = Draft202012Validator(
            cls.registry.contents(cls.schema_id),
            registry=cls.registry,
            format_checker=Draft202012Validator.FORMAT_CHECKER,
        )

    def test_all_schema_references_resolve_locally(self):
        for uri, schema in self.schemas.items():
            resolver = self.registry.resolver(base_uri=uri)
            for reference in references(schema):
                with self.subTest(schema=uri, reference=reference):
                    resolver.lookup(reference)

    def test_unknown_reference_is_rejected(self):
        unknown = f"{SCHEMA_BASE}missing.schema.json"
        with self.assertRaises(Unresolvable):
            self.registry.resolver(base_uri=self.schema_id).lookup(unknown)

        validator = self.validator.evolve(schema={"$schema": DRAFT, "$ref": unknown})
        with self.assertRaises(Unresolvable):
            validator.validate({})

    def assert_outcome(self, fixture_id, data, expected):
        try:
            instance = strict_json(data)
        except JsonParseError as error:
            stage = "parse"
            categories = (error.category,)
            locations = ("#",)
        else:
            errors = list(self.validator.iter_errors(instance))
            stage = "schema" if errors else "ok"
            categories = tuple(error.validator for error in errors) or ("accepted",)
            locations = tuple(schema_location(error.absolute_schema_path) for error in errors)
        diagnostic = (
            f"schema={self.schema_path} fixture={fixture_id} phase={stage} "
            f"categories={categories} schema_paths={locations}"
        )
        self.assertEqual(expected["stage"], stage, diagnostic)
        self.assertIn(expected["category"], categories, diagnostic)

    def test_shared_vectors(self):
        cases = strict_json(CASE_FILE.read_bytes())
        self.assertTrue(cases, f"{CASE_FILE}: no shared vectors")
        seen = set()
        for case in cases:
            fixture_id = case["id"]
            with self.subTest(fixture=fixture_id):
                self.assertNotIn(fixture_id, seen, f"{CASE_FILE}: duplicate fixture ID")
                seen.add(fixture_id)
                self.assert_outcome(
                    fixture_id, case["raw"].encode("utf-8"), case["expected"]
                )

    def test_non_utf8_is_rejected_before_schema_validation(self):
        self.assert_outcome(
            "synthetic_invalid_utf8",
            b'{"resolved_config":{"label":"\xff"}}',
            {"stage": "parse", "category": "invalid_utf8"},
        )


if __name__ == "__main__":
    unittest.main()
