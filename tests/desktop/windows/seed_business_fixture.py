"""Seed disposable native UI inputs; never point this at an existing directory."""

from hashlib import sha256
import json
from pathlib import Path
import sys

from PIL import Image, ImageDraw


def seed(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=False)
    for name in ("Workspace", "Inputs", "Exports", "Backups"):
        (root / name).mkdir()
    image = Image.new("RGB", (200, 100), (24, 50, 72))
    ImageDraw.Draw(image).rectangle((30, 20, 69, 49), fill=(240, 80, 24))
    image.save(root / "Inputs" / "signal.png")
    document = {
        "images": [{"id": 1, "file_name": "signal.png", "width": 200, "height": 100}],
        "categories": [{"id": 8, "name": "signal"}],
        "annotations": [{"id": 1, "image_id": 1, "category_id": 8,
                         "bbox": [190, 20, 40, 30], "area": 1200, "iscrowd": 0}],
    }
    (root / "Inputs" / "signal.json").write_text(json.dumps(document), encoding="utf-8")
    proof = {file.name: sha256(file.read_bytes()).hexdigest()
             for file in sorted((root / "Inputs").iterdir())}
    (root / "input-proof.json").write_text(json.dumps(proof, indent=2), encoding="utf-8")
    print(root.resolve())


if __name__ == "__main__":
    seed(Path(sys.argv[1]))
