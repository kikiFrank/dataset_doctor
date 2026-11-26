"""Round-trip test bytes through the production Python Protobuf bindings."""

import json
from pathlib import Path
import sys

repo = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(repo / "services/python-worker/src/dd_worker/generated"))
from datasetdoctor.v1.common_pb2 import ArtifactRef

message = ArtifactRef.FromString(sys.stdin.buffer.read())
fields = {descriptor.name: str(value) for descriptor, value in message.ListFields()}
print(json.dumps({"fields": fields, "wire_hex": message.SerializeToString().hex()}))
