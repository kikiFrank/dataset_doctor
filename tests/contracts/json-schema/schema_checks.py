"""Shared offline checks for the Draft 2020-12 contract suites."""

import json
import math
from pathlib import Path

from jsonschema import Draft202012Validator
from jsonschema.exceptions import SchemaError
from referencing import Registry, Resource
from referencing.exceptions import NoSuchResource


REPO = Path(__file__).resolve().parents[3]
SCHEMA_DIR = REPO / "contracts/json-schema/v1"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
SCHEMA_BASE = "https://datasetdoctor.invalid/contracts/json-schema/v1/"


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


def bounded_int(token):
    try:
        return int(token)
    except ValueError:
        # The JSON lexer has already verified the base-10 token.
        raise JsonParseError("integer_too_long") from None


def strict_json(data):
    try:
        text = data.decode("utf-8", errors="strict")
        return json.loads(
            text,
            object_pairs_hook=unique_object,
            parse_constant=reject_constant,
            parse_float=finite_float,
            parse_int=bounded_int,
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


class SchemaVectorChecks:
    @classmethod
    def setUpClass(cls):
        cls.schemas = {}
        for name in ("common.schema.json", cls.schema_filename):
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
        cls.schema_id = f"{SCHEMA_BASE}{cls.schema_filename}"
        cls.schema_path = SCHEMA_DIR / cls.schema_filename
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
        cases = strict_json(self.case_file.read_bytes())
        self.assertTrue(cases, f"{self.case_file}: no shared vectors")
        seen = set()
        for case in cases:
            fixture_id = case["id"]
            with self.subTest(fixture=fixture_id):
                self.assertNotIn(fixture_id, seen, f"{self.case_file}: duplicate fixture ID")
                seen.add(fixture_id)
                self.assert_outcome(
                    fixture_id, case["raw"].encode("utf-8"), case["expected"]
                )
