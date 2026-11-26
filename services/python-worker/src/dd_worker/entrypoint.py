"""Local Worker process entry point. Bootstrap credentials come from stdin."""

from concurrent.futures import ThreadPoolExecutor
import hmac
import json
from pathlib import Path
import re
import sys
from threading import Event
from uuid import UUID

import grpc

# Generated imports use the canonical Protobuf path datasetdoctor/v1.
sys.path.insert(0, str(Path(__file__).resolve().with_name("generated")))
from datasetdoctor.v1 import worker_service_pb2 as protocol
from datasetdoctor.v1 import worker_service_pb2_grpc as service

PROTOCOL_MAJOR = 1
BUILD_VERSION = "0.1.0"
BOOTSTRAP_LIMIT = 4096
CONTROL_LIMIT = 4 * 1024 * 1024


def read_bootstrap(source):
    raw = source.readline(BOOTSTRAP_LIMIT + 1)
    if len(raw) > BOOTSTRAP_LIMIT or not raw.endswith(b"\n"):
        raise ValueError("invalid bootstrap framing")
    config = json.loads(raw.decode("utf-8"))
    if not isinstance(config, dict) or config.keys() != {
        "token", "executor_session_id", "protocol_major"
    }:
        raise ValueError("invalid bootstrap fields")
    if type(config["protocol_major"]) is not int or config["protocol_major"] != PROTOCOL_MAJOR:
        raise ValueError("unsupported protocol major")
    token = config["token"]
    session = config["executor_session_id"]
    if not isinstance(token, str) or re.fullmatch(r"[0-9a-f]{32}", token) is None:
        raise ValueError("invalid bootstrap token")
    if not isinstance(session, str) or str(UUID(session)) != session:
        raise ValueError("invalid executor session")
    return token, session


class Worker(service.PythonWorkerServicer):
    def __init__(self, token, session_id, stop):
        self._authorization = f"Bearer {token}"
        self._session_id = session_id
        self._stop = stop

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
        return protocol.HealthSnapshot(
            role="WORKER",
            executor_session_id=self._session_id,
            protocol_major=PROTOCOL_MAJOR,
            build_version=BUILD_VERSION,
            active_executions=0,
        )

    def Shutdown(self, request, context):
        self._authorize(context)
        self._stop.set()
        return protocol.ShutdownReceipt(accepted=True)


def main():
    try:
        token, session = read_bootstrap(sys.stdin.buffer)
    except ValueError:
        print("Worker rejected bootstrap", file=sys.stderr)
        return 2
    stop = Event()
    options = [
        ("grpc.max_receive_message_length", CONTROL_LIMIT),
        ("grpc.max_send_message_length", CONTROL_LIMIT),
    ]
    with ThreadPoolExecutor(max_workers=2) as pool:
        server = grpc.server(pool, options=options)
        service.add_PythonWorkerServicer_to_server(Worker(token, session, stop), server)
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
