"""Use the generated Worker-side client against the actual Core loopback server."""

import json
from pathlib import Path
import sys

import grpc

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "services/python-worker/src/dd_worker/generated"))
from datasetdoctor.v1 import core_execution_pb2 as protocol
from datasetdoctor.v1 import core_execution_pb2_grpc as rpc


def main():
    bootstrap = json.loads(sys.stdin.buffer.readline())
    with grpc.insecure_channel(bootstrap["address"]) as channel:
        handle = rpc.CoreExecutionControlStub(channel).ClaimGrant(
            protocol.ClaimGrantRequest(**bootstrap["claim"]),
            metadata=(("authorization", f"Bearer {bootstrap['credential']}"),),
            timeout=5,
        )
    print(json.dumps({
        "task_id": handle.task_id,
        "attempt_id": handle.attempt_id,
        "worker_execution_id": handle.worker_execution_id,
    }))


if __name__ == "__main__":
    main()
