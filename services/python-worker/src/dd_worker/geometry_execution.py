"""Run the approved per-image COCO geometry check in the Worker process."""

from annotation_geometry import audit_geometry
from image_inspection import ImageInspectionError, inspect_image
from worker_execution import (
    IMAGE_LIMIT, JSON_LIMIT, InputChanged, seal_result, strict_json, verified_bytes,
    verified_manifest_asset, verified_path, write_json,
)

CAPABILITY = "annotation_geometry.v1"
OPERATION_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartAnnotationGeometryAudit\0"


def compute_result(request):
    try:
        return run_geometry(request), None
    except InputChanged:
        return None, "input_changed"
    except ImageInspectionError as error:
        return None, error.code
    except (ValueError, TypeError, KeyError, IndexError, RecursionError):
        return None, "invalid_input"
    except OSError:
        return None, "file_io_failed"


def run_geometry(request):
    inputs = request.parameters
    expected = verified_manifest_asset(request, CAPABILITY)
    annotation_digest = inputs.annotation.sha256 if inputs.HasField("annotation") else None
    if expected["media_digest"] != inputs.image.sha256 or expected["annotation_digest"] != annotation_digest:
        raise ValueError("input does not match the manifest")
    image = inspect_image(verified_bytes(inputs.image, IMAGE_LIMIT), max_pixels=inputs.max_pixels)
    annotation = strict_json(verified_bytes(inputs.annotation, JSON_LIMIT)) if annotation_digest is not None else None
    checked = audit_geometry(annotation, width=image.width, height=image.height)
    directory = verified_path(inputs.output_directory)
    result = write_json(directory, "geometry-result.json", {
        "status": checked.status,
        "image": {"sha256": image.sha256, "width": image.width, "height": image.height},
        "findings": [
            {"code": item.code, "annotation_id": item.annotation_id} for item in checked.findings
        ],
    })
    return seal_result(request, directory, [dict(result, kind="CHECK_RESULT", media_type="application/json")])
