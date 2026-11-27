"""Check the real probe and exact frame artifacts produced by the Windows desktop."""

from contextlib import closing
from hashlib import sha256
from io import BytesIO
import json
from pathlib import Path
import sqlite3
import sys

from PIL import Image


def digest(content):
    return sha256(content).hexdigest()


def artifacts(workspace, task_id, relative_manifest, execution):
    path = (workspace / relative_manifest).resolve(strict=True)
    assert path.is_relative_to(workspace)
    raw = path.read_bytes()
    ref = json.loads(execution)["terminal_receipt"]["result_manifest_ref"]
    assert ref["owner"] == {"id": task_id, "kind": "TASK"}
    assert digest(raw) == ref["sha256"] and len(raw) == int(ref["size_bytes"])
    manifest = json.loads(raw)
    assert manifest["task_id"] == task_id
    by_kind = {}
    for entry in manifest["entries"]:
        artifact = (path.parent / entry["relative_path"]).resolve(strict=True)
        assert artifact.parent == path.parent
        data = artifact.read_bytes()
        assert digest(data) == entry["sha256"] and len(data) == int(entry["size_bytes"])
        by_kind[entry["kind"]] = data
    return by_kind


def verify(workspace, video, expected_frame, original_digest):
    with closing(sqlite3.connect((workspace / "workspace.db").as_uri() + "?mode=ro", uri=True)) as db:
        assert db.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        assert db.execute("PRAGMA foreign_key_check").fetchall() == []
        ((asset, media_digest, registered_path),) = db.execute(
            "SELECT asset_id, media_digest, path FROM assets WHERE kind='VIDEO' AND location_kind='EXTERNAL'",
        ).fetchall()
        assert Path(registered_path).samefile(video)
        assert media_digest == original_digest == digest(video.read_bytes())
        tasks = {}
        for task_id, capability, status, manifest, execution in db.execute(
            "SELECT task_id, capability_id, status, output_manifest_path, execution_metadata_json FROM tasks",
        ):
            assert status == "SUCCEEDED" and capability not in tasks
            tasks[capability] = artifacts(workspace, task_id, manifest, execution)
        assert set(tasks) == {"video_probe.v1", "frame_resolve.v1"}
    index = json.loads(tasks["video_probe.v1"]["VIDEO_FRAME_INDEX"])
    frame = json.loads(tasks["frame_resolve.v1"]["VIDEO_FRAME_REF"])
    pixels = tasks["frame_resolve.v1"]["VIDEO_FRAME_IMAGE"]
    assert len(index["frames"]) == 6
    assert [int(item["raw_pts"]) for item in index["frames"]] == [0, 500, 1000, 1500, 2000, 2500]
    assert index["source_video_id"] == frame["source_video_id"] == asset
    assert index["source_digest"] == frame["source_digest"] == original_digest
    assert frame["frame_index"] == "2" and frame["raw_pts"] == "1000"
    with Image.open(BytesIO(pixels)) as actual, Image.open(expected_frame) as expected:
        assert actual.size == expected.size == (32, 24)
        assert actual.convert("RGB").tobytes() == expected.convert("RGB").tobytes()
    evidence = {"source_sha256": original_digest, "frame_sha256": digest(pixels),
                "source_video_id": asset, "frame_index": 2, "pts_us": 1_000_000}
    (workspace.parent / "video-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    print("Relocated desktop Worker registered the clip and decoded the authoritative frame")


if __name__ == "__main__":
    verify(
        Path(sys.argv[1]).resolve(strict=True), Path(sys.argv[2]).resolve(strict=True),
        Path(sys.argv[3]).resolve(strict=True), sys.argv[4].lower(),
    )
