"""Check ArtifactRef with the locked protoc and standard-library byte streams."""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path


REPO = Path(__file__).resolve().parents[4]
PROTO_ROOT = REPO / "contracts/proto"
SOURCE = PROTO_ROOT / "datasetdoctor/v1/common.proto"
CASES = REPO / "testdata/contracts/proto/v1/artifact-ref/cases.json"
OUTPUT = REPO / "target/contract-check/p01-02"
MESSAGE = "datasetdoctor.v1.ArtifactRef"
VERSION = b"libprotoc 29.3"
DIAGNOSTICS = {
    "owner_conflict": r'another member of oneof "owner"',
    "unknown_field": r'has no field named "unknown_field"',
    "negative_uint64": r"Expected integer, got: -",
    "overflow_uint64": r"Integer out of range \(18446744073709551616\)",
}
FIELDS = {
    "artifact_id": (1, "TYPE_STRING", None, False),
    "owner_task_id": (2, "TYPE_STRING", "0", False),
    "kind": (3, "TYPE_STRING", None, False),
    "media_type": (4, "TYPE_STRING", None, False),
    "size_bytes": (5, "TYPE_UINT64", None, False),
    "sha256": (6, "TYPE_STRING", None, False),
    "schema_id": (7, "TYPE_STRING", "1", True),
    "owner_asset_id": (10, "TYPE_STRING", "0", False),
    "owner_review_id": (11, "TYPE_STRING", "0", False),
}


class CheckError(RuntimeError):
    pass


class PrerequisiteError(RuntimeError):
    pass


def require(condition, detail):
    if not condition:
        raise CheckError(detail)


def run_protoc(protoc, case, options, data=b"", *, prerequisite=False):
    command = [str(protoc), *map(str, options)]
    print(f"[{case}] > {subprocess.list2cmdline(command)}", flush=True)
    try:
        result = subprocess.run(
            command, input=data, capture_output=True, cwd=OUTPUT, timeout=30
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        error_type = PrerequisiteError if prerequisite else CheckError
        raise error_type(f"{case}: {error}") from None
    print(f"[{case}] exit_code={result.returncode}")
    stderr = result.stderr.decode("utf-8", errors="replace").strip()
    print(f"[{case}] stderr: {stderr or '(empty)'}")
    return result


def expect_success(case, result):
    require(result.returncode == 0, f"{case}: expected protoc exit 0")
    return result.stdout


def expect_rejection(case, result, pattern):
    stderr = result.stderr.decode("utf-8", errors="replace")
    require(
        result.returncode != 0 and not result.stdout and re.search(pattern, stderr),
        f"{case}: expected nonzero exit and the matching diagnostic",
    )
    print(f"[{case}] expected rejection confirmed")


def check_tools(protoc, google_root):
    version = run_protoc(protoc, "version", ["--version"], prerequisite=True)
    actual = version.stdout.decode("utf-8", errors="replace").strip()
    print(f"[version] stdout: {actual or '(empty)'}")
    if version.returncode != 0 or version.stdout.splitlines() != [VERSION]:
        raise PrerequisiteError("Expected libprotoc 29.3 before compilation")

    descriptor_proto = google_root / "google/protobuf/descriptor.proto"
    if not descriptor_proto.is_file():
        raise PrerequisiteError(f"Missing descriptor include: {descriptor_proto}")
    decode_options = (
        "--decode=google.protobuf.FileDescriptorSet",
        f"--proto_path={google_root}",
        descriptor_proto,
    )
    empty = run_protoc(protoc, "descriptor_include", decode_options, prerequisite=True)
    if empty.returncode != 0 or empty.stdout.strip():
        raise PrerequisiteError(f"Cannot decode with descriptor include: {descriptor_proto}")
    return decode_options


def read_text_fields(data):
    # The pinned compiler prints one scalar or block delimiter per line for
    # these descriptors and ASCII fixtures. Values remain strings, including uint64.
    stack = [{}]
    for entry in data.decode("utf-8").splitlines():
        line = entry.strip()
        if line == "}":
            require(len(stack) > 1, "Unexpected closing block in decoded TextFormat")
            stack.pop()
        elif line.endswith(" {"):
            record = {}
            stack[-1].setdefault(line[:-2], []).append(record)
            stack.append(record)
        elif line:
            name, separator, value = line.partition(": ")
            require(separator and value, "Unexpected scalar in decoded TextFormat")
            scalar = json.loads(value) if value.startswith('"') else value
            stack[-1].setdefault(name, []).append(scalar)
    require(len(stack) == 1, "Unclosed block in decoded TextFormat")
    return stack[0]


def one(record, name):
    values = record.get(name, [])
    require(len(values) == 1, f"Decoded TextFormat needs exactly one {name}")
    return values[0]


def check_descriptor(data):
    file = one(read_text_fields(data), "file")
    require(one(file, "name") == "datasetdoctor/v1/common.proto", "Unexpected descriptor file")
    require(one(file, "package") == "datasetdoctor.v1", "Unexpected descriptor package")
    require(one(file, "syntax") == "proto3", "Unexpected descriptor syntax")
    require(
        not (file.keys() & {"dependency", "service", "enum_type", "extension"}),
        "common.proto contains another declaration or import",
    )
    message = one(file, "message_type")
    require(one(message, "name") == "ArtifactRef", "Unexpected descriptor message")
    require(
        not (message.keys() & {"nested_type", "enum_type", "extension", "extension_range"}),
        "ArtifactRef has an extra declaration",
    )
    oneofs = [one(group, "name") for group in message.get("oneof_decl", [])]
    require(oneofs == ["owner", "_schema_id"], "Unexpected owner or optional presence group")
    actual = {}
    for field in message.get("field", []):
        name = one(field, "name")
        require(name not in actual, f"Duplicate descriptor field: {name}")
        require(one(field, "label") == "LABEL_OPTIONAL", f"Unexpected label: {name}")
        actual[name] = (
            int(one(field, "number")),
            one(field, "type"),
            one(field, "oneof_index") if "oneof_index" in field else None,
            field.get("proto3_optional") == ["true"],
        )
    require(actual == FIELDS, f"ArtifactRef descriptor differs from C5.5: {actual}")
    print("[descriptor] proto3, nine fields, owner branches and schema_id presence verified")


def compile_descriptor(protoc, case, source, proto_root, destination):
    result = run_protoc(
        protoc,
        case,
        (f"--proto_path={proto_root}", f"--descriptor_set_out={destination}",
         "--include_imports", source),
    )
    expect_success(case, result)
    require(destination.is_file(), f"{case}: descriptor was not generated")
    return destination.read_bytes()


def check_vector_cases(protoc):
    cases = json.loads(CASES.read_text(encoding="utf-8"))
    require(cases["message"] == MESSAGE, "Unexpected fixture message type")
    all_ids = [case["id"] for case in cases["valid"] + cases["invalid"]]
    require(len(all_ids) == len(set(all_ids)), "Duplicate fixture ID")
    encode = (f"--encode={MESSAGE}", f"--proto_path={PROTO_ROOT}", SOURCE)
    decode = (f"--decode={MESSAGE}", f"--proto_path={PROTO_ROOT}", SOURCE)
    observed = []
    for case in cases["valid"]:
        case_id = case["id"]
        text = case["text_format"].encode("utf-8")
        wire = expect_success(f"{case_id}/encode", run_protoc(protoc, f"{case_id}/encode", encode, text))
        require(wire == bytes.fromhex(case["wire_hex"]), f"{case_id}: differs from independent wire")
        result = run_protoc(protoc, f"{case_id}/decode", decode, wire)
        decoded = read_text_fields(expect_success(f"{case_id}/decode", result))
        fields = {name: one(decoded, name) for name in decoded}
        require(fields == case["expected_fields"], f"{case_id}: decoded fields or presence differ")
        observed.append(fields)
        print(f"[{case_id}] wire and decoded fields match, size_bytes={fields['size_bytes']}")
    require(
        {name for fields in observed for name in fields if name.startswith("owner_")}
        == {"owner_task_id", "owner_asset_id", "owner_review_id"},
        "The fixture must cover all three owner branches",
    )
    require(
        {"9007199254740993", "18446744073709551615"}
        <= {fields.get("size_bytes") for fields in observed},
        "The fixture must cover both documented uint64 boundaries",
    )
    require(
        {None, ""} <= {fields.get("schema_id") for fields in observed},
        "The fixture must cover absent and explicitly empty schema_id",
    )
    require(
        {case["expected_error"] for case in cases["invalid"]} == DIAGNOSTICS.keys(),
        "Fixture rejection categories differ from the work order",
    )
    for case in cases["invalid"]:
        result = run_protoc(protoc, case["id"], encode, case["text_format"].encode("utf-8"))
        expect_rejection(case["id"], result, DIAGNOSTICS[case["expected_error"]])


def run_checks(protoc, google_root):
    decode = check_tools(protoc, google_root)
    source_bytes = SOURCE.read_bytes()
    print(f"[source] {SOURCE} sha256={hashlib.sha256(source_bytes).hexdigest()}")
    with tempfile.TemporaryDirectory(prefix="run-", dir=OUTPUT) as temporary:
        work = Path(temporary)
        primary = work / "common.pb"
        repeat = work / "common-repeat.pb"
        first = compile_descriptor(protoc, "compile", SOURCE, PROTO_ROOT, primary)
        second = compile_descriptor(protoc, "compile_repeat", SOURCE, PROTO_ROOT, repeat)
        require(first == second, "Repeated descriptor compilation produced different bytes")
        print(f"[descriptor_drift] identical sha256={hashlib.sha256(first).hexdigest()}")
        decoded = run_protoc(protoc, "descriptor/decode", decode, first)
        check_descriptor(expect_success("descriptor/decode", decoded))
        check_vector_cases(protoc)

        invalid_source = work / "duplicate_field.proto"
        invalid_source.write_text(
            'syntax = "proto3"; package datasetdoctor.v1; message Duplicate { '
            'uint64 size_bytes = 5; string sha256 = 5; }\n',
            encoding="utf-8",
        )
        duplicate = run_protoc(
            protoc,
            "duplicate_field_number",
            (f"--proto_path={work}", f"--descriptor_set_out={work / 'invalid.pb'}", invalid_source),
        )
        expect_rejection(
            "duplicate_field_number", duplicate, r"Field number 5 has already been used"
        )
        require(SOURCE.read_bytes() == source_bytes, "common.proto changed during validation")
        for descriptor in (primary, repeat):
            descriptor.replace(OUTPUT / descriptor.name)
    print(f"[result] PASS: {OUTPUT / primary.name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--protoc", type=Path, required=True)
    parser.add_argument("--google-proto-root", type=Path, required=True)
    args = parser.parse_args()
    try:
        run_checks(args.protoc, args.google_proto_root)
    except PrerequisiteError as error:
        print(f"[result] PREREQUISITE FAILED: {error}", file=sys.stderr)
        return 2
    except (CheckError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"[result] CHECK FAILED: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
