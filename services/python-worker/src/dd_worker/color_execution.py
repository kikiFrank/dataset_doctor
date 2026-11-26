"""Seal the Worker color candidate, mask and exact parameters for Core."""

from color_repair import (
    ColorOperation, ColorRepairError, HueGate, Polygon, Rectangle, render_color_candidate,
)
from datasetdoctor.v1 import worker_service_pb2 as wire
from worker_execution import (
    IMAGE_LIMIT, InputChanged, seal_result, verified_bytes, verified_manifest_asset, verified_path,
    write_bytes, write_json,
)

CAPABILITY = "color_preview.v1"
OPERATION_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartColorPreview\0"
METHODS = {
    wire.COLOR_METHOD_HUE_REMAP: "HUE_REMAP",
    wire.COLOR_METHOD_SOLID_FILL: "SOLID_FILL",
}
FAILURES = {
    "source_changed": "input_changed",
    "decode_failed": "decode_failed",
    "pixel_limit_exceeded": "pixel_limit_exceeded",
    "unsupported_color_input": "unsupported_format",
}


def compute_result(request):
    try:
        return _render(request), None
    except ColorRepairError as error:
        return None, FAILURES.get(error.code, "invalid_input")
    except InputChanged:
        return None, "input_changed"
    except (ValueError, TypeError, KeyError, IndexError, RecursionError):
        return None, "invalid_input"
    except OSError:
        return None, "file_io_failed"


def _render(request):
    inputs = request.parameters
    expected = verified_manifest_asset(request, CAPABILITY)
    if expected["media_digest"] != inputs.image.sha256:
        raise ValueError("image does not match its approved manifest")
    region_kind = inputs.WhichOneof("region")
    if region_kind == "rectangle" and inputs.rectangle.HasField("origin"):
        rect = inputs.rectangle
        region = Rectangle(rect.origin.x, rect.origin.y, rect.width, rect.height)
        region_metadata = {"kind": "RECT", "bbox_xywh": [region.x, region.y, region.width, region.height]}
    elif region_kind == "polygon":
        region = Polygon(tuple((point.x, point.y) for point in inputs.polygon.points))
        region_metadata = {"kind": "POLYGON", "points_xy": region.points}
    else:
        raise ValueError("missing preview region")
    preferences = inputs.operation
    gate = None
    if preferences.HasField("source_color_gate"):
        source_gate = preferences.source_color_gate
        gate = HueGate(source_gate.hue_min_deg, source_gate.hue_max_deg, source_gate.wraps_zero)
    operation = ColorOperation(
        METHODS[preferences.method],
        tuple(preferences.target_rgb),
        preferences.strength if preferences.HasField("strength") else 1,
        preferences.feather_px,
        gate,
    )
    original = verified_bytes(inputs.image, IMAGE_LIMIT)
    candidate = render_color_candidate(original, inputs.image.sha256, region, operation,
                                       max_pixels=inputs.max_pixels)
    directory = verified_path(inputs.output_directory)
    jpeg = original.startswith(b"\xff\xd8")
    image = write_bytes(directory, "candidate.jpg" if jpeg else "candidate.png", candidate.encoded)
    mask = write_bytes(directory, "mask.png", candidate.mask_png)
    details = write_json(directory, "color-preview.json", {
        "asset_id": expected["asset_id"],
        "base_image_digest": expected["media_digest"],
        "base_annotation_digest": expected["annotation_digest"],
        "candidate_artifact_id": image["artifact_id"],
        "mask_artifact_id": mask["artifact_id"],
        "region": region_metadata,
        "operation": {
            "method": operation.method,
            "target_rgb": operation.target_rgb,
            "strength": operation.strength,
            "feather_px": operation.feather_px,
            "source_color_gate": None if gate is None else {
                "hue_min_deg": gate.minimum, "hue_max_deg": gate.maximum, "wraps_zero": gate.wraps_zero,
            },
        },
        "changed_pixels": candidate.changed_pixels,
        "lossy_reencode": candidate.lossy_reencode,
    })
    return seal_result(request, directory, [
        dict(image, kind="REPAIR_CANDIDATE", media_type="image/jpeg" if jpeg else "image/png"),
        dict(mask, kind="REPAIR_MASK", media_type="image/png"),
        dict(details, kind="REPAIR_PREVIEW", media_type="application/json"),
    ])
