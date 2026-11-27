"""Verify a desktop business run with SQLite, Pillow and the target COCO reader."""

import argparse
from contextlib import closing
from hashlib import sha256
from importlib.metadata import version
import json
from pathlib import Path
import sqlite3

from PIL import Image
from pycocotools.coco import COCO


def verify(root: Path, package: Path, color: bool = False, pointer_bbox: list[float] | None = None) -> None:
    assert version("pycocotools") == "2.0.11"
    proof = json.loads((root / "input-proof.json").read_text(encoding="utf-8"))
    source = root / "Inputs" / "signal.png"
    source_digest = sha256(source.read_bytes()).hexdigest()
    if color:
        assert source_digest != proof["signal.png"]
        with Image.open(source) as image:
            assert image.size == (200, 100)
            assert image.getpixel((0, 0)) == (26, 174, 102)
            assert image.getpixel((1, 0)) == (24, 50, 72)
            assert image.getpixel((40, 30)) == (240, 80, 24)
    else:
        assert source_digest == proof["signal.png"]
    database = root / "Workspace" / "workspace.db"
    assert database.is_file(), "Native creation did not publish the Workspace"
    with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as connection:
        assert connection.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        assert connection.execute("PRAGMA foreign_key_check").fetchall() == []
        assert connection.execute("SELECT COUNT(*) FROM dataset_assets").fetchone() == (1,)
        tasks = connection.execute("SELECT task_id,operation_kind,status FROM tasks ORDER BY created_at_ms").fetchall()
        for operation in ("DATASET_IMPORT_INSPECT", "DATASET_IMPORT_EXTERNAL",
                          "ANNOTATION_GEOMETRY", "REVIEW_COMMIT", "DATASET_EXPORT"):
            assert any(kind == operation and state == "SUCCEEDED" for _, kind, state in tasks), operation
        rows = connection.execute("""SELECT r.status,r.confirmed_annotation_digest,a.annotation_path,a.media_digest
            FROM dataset_assets d JOIN review_sessions r ON r.review_id=d.final_review_id
            JOIN assets a ON a.asset_id=d.asset_id""").fetchall()
        assert len(rows) == 1 and rows[0][0] == ("STALE" if color else "CONFIRMED"), rows
        _, digest, formal, recorded_media = rows[0]
        assert recorded_media == source_digest
        formal = Path(formal)
        if not formal.is_absolute():
            formal = database.parent / formal
        assert sha256(formal.read_bytes()).hexdigest() == digest
        if color:
            assert sum(kind == "ANNOTATION_GEOMETRY" and state == "SUCCEEDED"
                       for _, kind, state in tasks) == 3
            assert [(kind, state) for _, kind, state in tasks if kind.startswith("COLOR_")] == [
                ("COLOR_PREVIEW", "SUCCEEDED"), ("COLOR_APPLY", "SUCCEEDED")
            ]
            preview_path, = connection.execute(
                "SELECT output_manifest_path FROM tasks WHERE operation_kind='COLOR_PREVIEW'"
            ).fetchone()
            preview_directory = (database.parent / preview_path).parent
            assert (preview_directory / "candidate.png").read_bytes() == source.read_bytes()
            with Image.open(preview_directory / "mask.png") as mask:
                assert mask.size == (200, 100) and mask.getpixel((0, 0)) == 255 and mask.getpixel((1, 0)) == 0
            result, = connection.execute("SELECT result_json FROM file_operations WHERE operation_kind='COLOR_APPLY'").fetchone()
            receipt = json.loads(result)
            assert receipt["old_image_digest"] == proof["signal.png"] and receipt["image_digest"] == source_digest
    reader = COCO(str(package / "annotations.json"))
    images = reader.loadImgs(reader.getImgIds())
    assert len(images) == 1 and len(reader.anns) == 1
    assert {category["name"] for category in reader.cats.values()} == {"signal"}
    media = images[0]
    relative = Path(media["file_name"])
    assert not relative.is_absolute() and relative.parts[0] == "images" and ".." not in relative.parts
    with Image.open(package / relative) as image:
        image.load()
        assert image.size == (media["width"], media["height"]) == (200, 100)
        assert image.getpixel((40, 30)) == (240, 80, 24)
    assert sha256((package / relative).read_bytes()).hexdigest() == proof["signal.png"]
    annotation = next(iter(reader.anns.values()))
    assert annotation["image_id"] == media["id"]
    assert annotation["category_id"] in reader.cats
    if pointer_bbox is None:
        assert annotation["bbox"] == [30, 20, 40, 30]
        assert annotation["area"] == 1200
    else:
        # Actual mouse input is quantized to screen pixels. Compare to the
        # independently derived image-space target within one original pixel.
        assert len(annotation["bbox"]) == 4
        assert all(abs(actual - expected) <= 1 for actual, expected in zip(annotation["bbox"], pointer_bbox, strict=True))
        x, y, width, height = annotation["bbox"]
        assert 0 <= x and 0 <= y and 0 < width <= 200 - x and 0 < height <= 100 - y
        assert abs(annotation["area"] - width * height) < 1e-6
    result = {"platform": "Windows", "target_reader": version("pycocotools"),
              "workspace": str(database.parent), "coco_package": str(package),
              "tasks": tasks, "confirmed_annotation_digest": digest, "current_image_digest": source_digest,
              "color_applied": color}
    (root / "evidence.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print("Native business output verified with the actual COCO reader")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    parser.add_argument("package", type=Path)
    parser.add_argument("--color", action="store_true")
    parser.add_argument("--pointer-bbox", nargs=4, type=float)
    arguments = parser.parse_args()
    verify(arguments.root, arguments.package, arguments.color, arguments.pointer_bbox)
