"""Verify a desktop business run with SQLite, Pillow and the target COCO reader."""

from contextlib import closing
from hashlib import sha256
from importlib.metadata import version
import json
from pathlib import Path
import sqlite3
import sys

from PIL import Image
from pycocotools.coco import COCO


def verify(root: Path, package: Path) -> None:
    assert version("pycocotools") == "2.0.11"
    proof = json.loads((root / "input-proof.json").read_text(encoding="utf-8"))
    assert sha256((root / "Inputs" / "signal.png").read_bytes()).hexdigest() == proof["signal.png"]
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
        rows = connection.execute("""SELECT r.status,r.confirmed_annotation_digest,a.annotation_path
            FROM dataset_assets d JOIN review_sessions r ON r.review_id=d.final_review_id
            JOIN assets a ON a.asset_id=d.asset_id""").fetchall()
        assert len(rows) == 1 and rows[0][0] == "CONFIRMED", rows
        _, digest, formal = rows[0]
        formal = Path(formal)
        if not formal.is_absolute():
            formal = database.parent / formal
        assert sha256(formal.read_bytes()).hexdigest() == digest
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
    annotation = next(iter(reader.anns.values()))
    assert annotation["image_id"] == media["id"]
    assert annotation["category_id"] in reader.cats
    assert annotation["bbox"] == [30, 20, 40, 30]
    assert annotation["area"] == 1200
    result = {"platform": "Windows", "target_reader": version("pycocotools"),
              "workspace": str(database.parent), "coco_package": str(package),
              "tasks": tasks, "confirmed_annotation_digest": digest}
    (root / "evidence.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print("Native business output verified with the actual COCO reader")


if __name__ == "__main__":
    verify(Path(sys.argv[1]), Path(sys.argv[2]))
