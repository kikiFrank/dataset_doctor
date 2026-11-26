"""Profile exact inputs from an admitted dataset manifest and seal the evidence."""

from dataclasses import asdict
from uuid import UUID

from dataset_profile import ProfileReadFailure, ProfileSample, profile_dataset
from worker_execution import (
    IMAGE_LIMIT, JSON_LIMIT, InputChanged, seal_result, strict_json, verified_bytes,
    verified_manifest, verified_path, write_json,
)

CAPABILITY = "dataset_profile.v1"
OPERATION_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartProfileDataset\0"


def compute_profile(request):
    try:
        return run_profile(request), None
    except InputChanged:
        return None, "input_changed"
    except (ValueError, TypeError, KeyError, RecursionError):
        return None, "invalid_input"
    except OSError:
        return None, "file_io_failed"


def run_profile(request):
    inputs = request.parameters
    manifest = verified_manifest(request, CAPABILITY, asset_count=len(inputs.assets))
    if manifest["resolved_config"]["max_pixels"] != inputs.max_pixels:
        raise InputChanged("profile limit does not match its frozen input")
    selected = _samples(inputs.assets, manifest["assets"])
    profile = profile_dataset(selected, max_pixels=inputs.max_pixels)
    directory = verified_path(inputs.output_directory)
    body = {
        "input_fingerprint": request.context.input_fingerprint,
        "asset_count": profile.asset_count,
        "annotation_count": profile.annotation_count,
        "resolution_histogram": [
            {"width": width, "height": height, "count": count}
            for (width, height), count in profile.resolution_histogram.items()
        ],
        "class_counts": profile.class_counts,
        "empty_annotation_count": profile.empty_annotation_count,
        "unannotated_count": profile.unannotated_count,
    }
    errors = []
    if profile.file_errors:
        entry = _write(directory, "dataset-profile-file-errors.json", [asdict(error) for error in profile.file_errors])
        body["file_errors_ref"] = {
            "artifact_id": entry["artifact_id"],
            "owner": {"kind": "TASK", "id": request.context.task_id},
            "kind": entry["kind"], "media_type": entry["media_type"],
            "sha256": entry["sha256"], "size_bytes": str(entry["size_bytes"]),
        }
        errors.append(entry)
    result = _write(directory, "dataset-profile.json", body)
    return seal_result(request, directory, [result, *errors])


def _samples(bindings, expected):
    seen = set()
    for binding, asset in zip(bindings, expected, strict=True):
        asset_id = binding.asset_id
        if str(UUID(asset_id)) != asset_id or asset_id in seen or not binding.HasField("image"):
            raise ValueError("invalid profile asset binding")
        seen.add(asset_id)
        annotation = binding.annotation if binding.HasField("annotation") else None
        if (
            asset_id != asset["asset_id"] or binding.image.sha256 != asset["media_digest"]
            or (annotation.sha256 if annotation else None) != asset["annotation_digest"]
        ):
            raise InputChanged("profile asset does not match its frozen input")
        image = _read(binding.image, IMAGE_LIMIT)
        document = None
        if annotation is not None:
            encoded = _read(annotation, JSON_LIMIT)
            if isinstance(encoded, ProfileReadFailure):
                document = encoded
            else:
                try:
                    document = strict_json(encoded)
                    if not isinstance(document, dict):
                        raise ValueError("expected per-image COCO")
                except (ValueError, RecursionError):
                    document = ProfileReadFailure("invalid_annotation")
        yield ProfileSample(asset_id, image, document)


def _read(locator, limit):
    try:
        return verified_bytes(locator, limit)
    except FileNotFoundError:
        return ProfileReadFailure("offline")
    except OSError:
        return ProfileReadFailure("file_io_failed")


def _write(directory, name, body):
    return dict(write_json(directory, name, body), kind="CHECK_RESULT", media_type="application/json")
