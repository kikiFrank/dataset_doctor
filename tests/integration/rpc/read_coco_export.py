"""Read the actual exported package with the local COCO consumer."""

from importlib.metadata import version
from pathlib import Path
import sys

from PIL import Image
from pycocotools.coco import COCO


def main():
    package = Path(sys.argv[1])
    assert version("pycocotools") == "2.0.11"
    reader = COCO(str(package / "annotations.json"))
    images = reader.loadImgs(reader.getImgIds())
    assert len(images) == 2
    assert sorted(img["id"] for img in images) == [1, 2]
    assert {item["name"]: item["id"] for item in reader.loadCats(reader.getCatIds())} == {
        "helmet": 1,
        "signal": 2,
    }
    counts = []
    annotation_ids = []
    for media in images:
        relative = Path(media["file_name"])
        assert not relative.is_absolute() and relative.parts[0] == "images"
        assert ".." not in relative.parts
        with Image.open(package / relative) as image:
            image.load()
            assert image.size == (media["width"], media["height"]) == (2, 1)
        annotations = reader.loadAnns(reader.getAnnIds(imgIds=[media["id"]]))
        counts.append(len(annotations))
        for annotation in annotations:
            annotation_ids.append(annotation["id"])
            assert annotation["image_id"] == media["id"]
            assert annotation["category_id"] in reader.cats
        if annotations:
            assert sorted(ann["category_id"] for ann in annotations) == [1, 2]
    assert sorted(counts) == [0, 2]
    assert sorted(annotation_ids) == [1, 2]


if __name__ == "__main__":
    main()
