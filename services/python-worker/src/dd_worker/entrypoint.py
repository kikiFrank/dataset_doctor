"""Local Worker process entry point. Bootstrap credentials come from stdin."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import nullcontext
from functools import partial
import hmac
import json
from pathlib import Path
import re
import sys
from threading import Event
from uuid import UUID

import grpc

# Generated imports use the canonical Protobuf path datasetdoctor/v1.
worker_root = Path(__file__).resolve().parent
sys.path[:0] = [str(worker_root), str(worker_root / "generated")]
from datasetdoctor.v1 import worker_service_pb2 as protocol
from datasetdoctor.v1 import worker_service_pb2_grpc as service
import color_execution
import geometry_execution
import profile_execution
import video_execution
from worker_execution import WorkerExecutions

PROTOCOL_MAJOR = 1
BUILD_VERSION = "0.1.0"
BOOTSTRAP_LIMIT = 4096
CONTROL_LIMIT = 4 * 1024 * 1024


def read_bootstrap(source):
    raw = source.readline(BOOTSTRAP_LIMIT + 1)
    if len(raw) > BOOTSTRAP_LIMIT or not raw.endswith(b"\n"):
        raise ValueError("invalid bootstrap framing")
    config = json.loads(raw.decode("utf-8"))
    required = {"token", "executor_session_id", "protocol_major"}
    if not isinstance(config, dict) or not required <= config.keys() <= required | {"core"}:
        raise ValueError("invalid bootstrap fields")
    if type(config["protocol_major"]) is not int or config["protocol_major"] != PROTOCOL_MAJOR:
        raise ValueError("unsupported protocol major")
    token = config["token"]
    session = config["executor_session_id"]
    if not isinstance(token, str) or re.fullmatch(r"[0-9a-f]{32}", token) is None:
        raise ValueError("invalid bootstrap token")
    if not isinstance(session, str) or str(UUID(session)) != session:
        raise ValueError("invalid executor session")
    core = config.get("core")
    if "core" in config:
        if not isinstance(core, dict) or core.keys() != {"address", "claim_token"}:
            raise ValueError("invalid Core bootstrap fields")
        address = core["address"]
        claim_token = core["claim_token"]
        if (
            not isinstance(address, str) or re.fullmatch(r"127\.0\.0\.1:[1-9][0-9]{0,4}", address) is None
            or int(address.partition(":")[2]) > 65535
            or not isinstance(claim_token, str) or re.fullmatch(r"[0-9a-f]{32}", claim_token) is None
        ):
            raise ValueError("invalid Core bootstrap channel")
    return token, session, core


class Worker(service.PythonWorkerServicer):
    def __init__(self, token, session_id, stop, executions):
        self._authorization = f"Bearer {token}"
        self._session_id = session_id
        self._stop = stop
        self._executions = executions
        self._video_tools = video_execution.configured_tools()

    def _authorize(self, context):
        values = [
            entry.value for entry in context.invocation_metadata()
            if entry.key == "authorization"
        ]
        if (
            len(values) != 1
            or not isinstance(values[0], str)
            or not values[0].isascii()
            or not hmac.compare_digest(values[0], self._authorization)
        ):
            context.abort(grpc.StatusCode.UNAUTHENTICATED, "invalid Worker credential")

    def Health(self, request, context):
        self._authorize(context)
        capabilities = []
        if self._executions is not None:
            capabilities.extend([geometry_execution.CAPABILITY, color_execution.CAPABILITY, profile_execution.CAPABILITY])
            if self._video_tools is not None:
                capabilities.extend([
                    video_execution.CAPABILITY, video_execution.RESOLVE_CAPABILITY, video_execution.TRACK_CAPABILITY,
                ])
        return protocol.HealthSnapshot(
            role="WORKER",
            executor_session_id=self._session_id,
            protocol_major=PROTOCOL_MAJOR,
            build_version=BUILD_VERSION,
            loaded_capability_ids=capabilities,
            active_executions=self._executions.active_count() if self._executions is not None else 0,
        )

    def StartAnnotationGeometryAudit(self, request, context):
        return self._start(
            request, context, geometry_execution.OPERATION_DOMAIN, geometry_execution.compute_result,
        )

    def StartProfileDataset(self, request, context):
        return self._start(
            request, context, profile_execution.OPERATION_DOMAIN, profile_execution.compute_profile, None,
        )

    def StartColorPreview(self, request, context):
        return self._start(
            request, context, color_execution.OPERATION_DOMAIN, color_execution.compute_result,
        )

    def StartVideoProbe(self, request, context):
        return self._start(
            request, context, video_execution.PROBE_DOMAIN,
            partial(video_execution.compute_probe, tools=self._video_tools), "source_video",
            available=self._video_tools is not None,
        )

    def StartFrameResolve(self, request, context):
        return self._start(
            request, context, video_execution.RESOLVE_DOMAIN,
            partial(video_execution.compute_resolve, tools=self._video_tools), "source_video",
            available=self._video_tools is not None,
        )

    def StartTracking(self, request, context):
        return self._start(
            request, context, video_execution.TRACK_DOMAIN,
            partial(video_execution.compute_tracking, tools=self._video_tools), "source_video",
            available=self._video_tools is not None,
        )

    def _start(self, request, context, operation_domain, compute, source_field="image", *, available=True):
        self._authorize(context)
        if not available or self._executions is None or self._stop.is_set():
            context.abort(grpc.StatusCode.FAILED_PRECONDITION, "Worker cannot admit executions")
        return self._executions.start(request, context, operation_domain, compute, source_field)

    def GetExecution(self, request, context):
        self._authorize(context)
        if self._executions is None:
            context.abort(grpc.StatusCode.NOT_FOUND, "execution not found")
        return self._executions.snapshot(request, context)

    def Shutdown(self, request, context):
        self._authorize(context)
        self._stop.set()
        return protocol.ShutdownReceipt(accepted=True)


def main():
    try:
        token, session, core = read_bootstrap(sys.stdin.buffer)
    except ValueError:
        print("Worker rejected bootstrap", file=sys.stderr)
        return 2
    stop = Event()
    options = [
        ("grpc.max_receive_message_length", CONTROL_LIMIT),
        ("grpc.max_send_message_length", CONTROL_LIMIT),
    ]
    channel = grpc.insecure_channel(core["address"], options=options) if core else nullcontext()
    with ThreadPoolExecutor(max_workers=2) as pool, ThreadPoolExecutor(max_workers=1) as compute, channel as connection:
        server = grpc.server(pool, options=options)
        executions = (
            WorkerExecutions(connection, core["claim_token"], session, compute)
            if core else None
        )
        service.add_PythonWorkerServicer_to_server(Worker(token, session, stop, executions), server)
        port = server.add_insecure_port("127.0.0.1:0")
        server.start()
        try:
            print(json.dumps({"port": port}), flush=True)
            stop.wait()
        finally:
            server.stop(grace=1).wait()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
