"""Run the approved per-image COCO geometry check in the Worker process."""

from dataclasses import dataclass
from datetime import UTC, datetime
from hashlib import sha256
import json
from math import isfinite
import os
from pathlib import Path
import tempfile
from threading import Lock
from uuid import UUID, uuid4

import grpc

from annotation_geometry import audit_geometry
from image_inspection import ImageInspectionError, inspect_image
from datasetdoctor.v1 import common_pb2, core_execution_pb2, core_execution_pb2_grpc
from datasetdoctor.v1 import worker_service_pb2 as protocol

CAPABILITY = "annotation_geometry.v1"
OPERATION_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartAnnotationGeometryAudit\0"
JSON_LIMIT = 16 * 1024 * 1024
IMAGE_LIMIT = 256 * 1024 * 1024


@dataclass(slots=True)
class Execution:
    request_digest: bytes
    handle: core_execution_pb2.ExecutionHandle
    status: str = "QUEUED"
    manifest: common_pb2.ArtifactRef | None = None
    error_code: str = ""


class GeometryExecutions:
    def __init__(self, channel, token, session, pool):
        self._core = core_execution_pb2_grpc.CoreExecutionControlStub(channel)
        self._authorization = (("authorization", f"Bearer {token}"),)
        self._session = session
        self._pool = pool
        self._lock = Lock()
        self._executions = {}

    def active_count(self):
        with self._lock:
            return sum(item.status in {"QUEUED", "RUNNING"} for item in self._executions.values())

    def start(self, request, rpc):
        context = request.context
        inputs = request.parameters
        manifest = request.input_manifest_ref
        try:
            for identifier in (
                context.workspace_id, context.workspace_session_id, context.task_id,
                context.attempt_id, context.grant_id, manifest.artifact_id,
            ):
                if str(UUID(identifier)) != identifier:
                    raise ValueError("identifier")
            if (
                not inputs.HasField("image") or not inputs.HasField("input_manifest")
                or not inputs.max_pixels or not inputs.output_directory
                or manifest.WhichOneof("owner") != "owner_task_id"
                or manifest.owner_task_id != context.task_id or manifest.kind != "INPUT_MANIFEST"
                or manifest.sha256 != inputs.input_manifest.sha256
            ):
                raise ValueError("input")
        except ValueError:
            rpc.abort(grpc.StatusCode.INVALID_ARGUMENT, "invalid geometry audit request")
        if context.executor_session_id != self._session:
            rpc.abort(grpc.StatusCode.PERMISSION_DENIED, "executor session does not match")

        key = (context.task_id, context.attempt_id)
        request_digest = sha256(request.SerializeToString(deterministic=True)).digest()
        operation_digest = sha256(OPERATION_DOMAIN + inputs.SerializeToString(deterministic=True)).hexdigest()
        with self._lock:
            existing = self._executions.get(key)
            if existing is not None:
                if existing.request_digest != request_digest:
                    rpc.abort(grpc.StatusCode.ALREADY_EXISTS, "attempt has a different request")
                return existing.handle
            try:
                handle = self._core.ClaimGrant(core_execution_pb2.ClaimGrantRequest(
                    grant_id=context.grant_id,
                    workspace_session_id=context.workspace_session_id,
                    task_id=context.task_id,
                    attempt_id=context.attempt_id,
                    executor_session_id=self._session,
                    worker_execution_id=str(uuid4()),
                    input_fingerprint=context.input_fingerprint,
                    operation_digest=operation_digest,
                ), metadata=self._authorization, timeout=5)
            except grpc.RpcError as error:
                rpc.abort(error.code(), "Core rejected the execution claim")
            execution = Execution(request_digest, handle)
            self._executions[key] = execution
            self._pool.submit(self._run, request, execution)
            return handle

    def snapshot(self, lookup, rpc):
        with self._lock:
            item = self._executions.get((lookup.task_id, lookup.attempt_id))
            if item is None:
                rpc.abort(grpc.StatusCode.NOT_FOUND, "execution not found")
            return protocol.ExecutionSnapshot(
                handle=item.handle, status=item.status,
                result_manifest_ref=item.manifest, error_code=item.error_code,
            )

    def _run(self, request, item):
        with self._lock:
            item.status = "RUNNING"
        try:
            manifest = run_geometry(request)
        except ImageInspectionError as error:
            self._failed(item, error.code)
        except (ValueError, TypeError, KeyError, IndexError, RecursionError):
            self._failed(item, "invalid_input")
        except OSError:
            self._failed(item, "file_io_failed")
        else:
            with self._lock:
                item.manifest = manifest
                item.status = "SUCCEEDED"

    def _failed(self, item, code):
        with self._lock:
            item.error_code = code
            item.status = "FAILED"


def run_geometry(request):
    task = request.context
    inputs = request.parameters
    manifest_data = verified_bytes(inputs.input_manifest, JSON_LIMIT)
    manifest = strict_json(manifest_data)
    if (
        len(manifest_data) != request.input_manifest_ref.size_bytes
        or manifest["input_fingerprint"] != task.input_fingerprint
        or manifest["capability_id"] != CAPABILITY or len(manifest["assets"]) != 1
    ):
        raise ValueError("invalid input manifest")
    expected = manifest["assets"][0]
    annotation_digest = inputs.annotation.sha256 if inputs.HasField("annotation") else None
    if expected["media_digest"] != inputs.image.sha256 or expected["annotation_digest"] != annotation_digest:
        raise ValueError("input does not match the manifest")
    image = inspect_image(verified_bytes(inputs.image, IMAGE_LIMIT), max_pixels=inputs.max_pixels)
    annotation = strict_json(verified_bytes(inputs.annotation, JSON_LIMIT)) if annotation_digest is not None else None
    checked = audit_geometry(annotation, width=image.width, height=image.height)
    directory = verified_path(inputs.output_directory)
    result = write_json(directory, "geometry-result.json", {
        "status": checked.status,
        "findings": [
            {"code": item.code, "annotation_id": item.annotation_id} for item in checked.findings
        ],
    })
    result_manifest = write_json(directory, "result-manifest.json", {
        "manifest_contract_version": 1,
        "task_id": task.task_id,
        "attempt_id": task.attempt_id,
        "input_fingerprint": task.input_fingerprint,
        "entries": [dict(result, kind="CHECK_RESULT", media_type="application/json")],
        "sealed_at": datetime.now(UTC).isoformat(),
    })
    return common_pb2.ArtifactRef(
        artifact_id=result_manifest["artifact_id"], owner_task_id=task.task_id,
        kind="TASK_RESULT_MANIFEST", media_type="application/json",
        sha256=result_manifest["sha256"], size_bytes=result_manifest["size_bytes"],
    )


def verified_path(name):
    path = Path(name)
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError("expected an already-resolved absolute path")
    return path


def verified_bytes(locator, limit):
    path = verified_path(locator.path)
    if not path.is_file():
        raise ValueError("expected a regular source file")
    with path.open("rb") as source:
        contents = source.read(limit + 1)
    if len(contents) > limit or sha256(contents).hexdigest() != locator.sha256:
        raise ValueError("source length or digest does not match")
    return contents


def strict_json(contents):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result

    def finite(raw):
        value = float(raw)
        if not isfinite(value):
            raise ValueError("non-finite number")
        return value

    return json.loads(contents.decode("utf-8"), object_pairs_hook=unique, parse_float=finite, parse_constant=finite)


def write_json(directory, filename, value):
    data = json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")
    with tempfile.NamedTemporaryFile(dir=directory, prefix=".worker-", delete=False) as temporary:
        staged = Path(temporary.name)
        try:
            temporary.write(data)
            temporary.flush()
            os.fsync(temporary.fileno())
        except BaseException:
            temporary.close()
            staged.unlink(missing_ok=True)
            raise
    try:
        os.link(staged, directory / filename)
    finally:
        staged.unlink(missing_ok=True)
    return {
        "artifact_id": str(uuid4()), "relative_path": filename,
        "size_bytes": len(data), "sha256": sha256(data).hexdigest(),
    }
