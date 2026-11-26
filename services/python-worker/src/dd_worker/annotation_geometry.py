"""Geometry and reference checks for the single-image COCO bbox profile."""

from dataclasses import dataclass
from math import isfinite


@dataclass(frozen=True, slots=True)
class GeometryFinding:
    code: str
    annotation_id: int | None = None


@dataclass(frozen=True, slots=True)
class GeometryAudit:
    status: str
    findings: tuple[GeometryFinding, ...]


def audit_geometry(document: dict | None, *, width: int, height: int) -> GeometryAudit:
    """Check a structurally validated annotation against decoded display dimensions."""
    if document is None:
        return GeometryAudit("NOT_EXECUTED", (GeometryFinding("annotation_missing"),))
    findings = []
    image = document["images"][0]
    if (image["width"], image["height"]) != (width, height):
        findings.append(GeometryFinding("dimension_mismatch"))

    categories = {item["id"] for item in document["categories"]}
    if len(categories) != len(document["categories"]):
        findings.append(GeometryFinding("duplicate_category_id"))
    annotations = document["annotations"]
    if not annotations:
        findings.append(GeometryFinding("annotation_empty"))

    seen = set()
    for item in annotations:
        local_id = item["id"]
        if local_id in seen:
            findings.append(GeometryFinding("duplicate_annotation_id", local_id))
        seen.add(local_id)
        if item["image_id"] != image["id"]:
            findings.append(GeometryFinding("image_reference_mismatch", local_id))
        if item["category_id"] not in categories:
            findings.append(GeometryFinding("unknown_category", local_id))
        issue = _bbox_issue(item["bbox"], width, height)
        if issue:
            findings.append(GeometryFinding(issue, local_id))

    return GeometryAudit("COMPLETED", tuple(findings))


def _bbox_issue(bbox, width, height):
    if len(bbox) != 4 or not all(_finite_coordinate(value) for value in bbox):
        return "invalid_bbox"
    x, y, box_width, box_height = bbox
    if box_width <= 0 or box_height <= 0:
        return "invalid_bbox"
    if x < 0 or y < 0 or x > width or y > height:
        return "bbox_out_of_bounds"
    if box_width > width - x or box_height > height - y:
        return "bbox_out_of_bounds"
    return None


def _finite_coordinate(value):
    return type(value) is int or (type(value) is float and isfinite(value))
