"""Run local image quality checks against the exact granted file and thresholds."""

from dataclasses import asdict

from image_inspection import ImageInspectionError, decode_display_image
from image_quality import ImageQualityPolicy, measure_image_quality
from worker_execution import (
    IMAGE_LIMIT, InputChanged, seal_result, verified_bytes, verified_manifest, verified_path, write_json,
)

CAPABILITY = "image_quality.v1"
OPERATION_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartImageQualityInspection\0"
PIXEL_CHECKS = (
    ("image.underexposed", "dark_fraction", "no_visible_pixels"),
    ("image.overexposed", "bright_fraction", "no_visible_pixels"),
    ("image.blur", "gradient_energy", "no_visible_neighbor_pairs"),
)


def compute_quality(request):
    try:
        return run_quality(request), None
    except InputChanged:
        return None, "input_changed"
    except (ValueError, TypeError, KeyError, RecursionError):
        return None, "invalid_input"
    except OSError:
        return None, "file_io_failed"


def run_quality(request):
    inputs = request.parameters
    manifest = verified_manifest(request, CAPABILITY)
    if not inputs.HasField("policy"):
        raise ValueError("image quality policy is required")
    parameters = inputs.policy
    policy = ImageQualityPolicy(parameters.dark_luma_max, parameters.bright_luma_min,
                                parameters.exposure_fraction, parameters.min_gradient_energy)
    policy.validate()
    config = {"max_pixels": inputs.max_pixels, "quality_policy": asdict(policy)}
    if manifest["assets"][0]["media_digest"] != inputs.image.sha256 or manifest["resolved_config"] != config:
        raise InputChanged("image quality input differs from the frozen manifest")

    image, measurements, checks = _inspect(inputs, policy)
    directory = verified_path(inputs.output_directory)
    result = write_json(directory, "image-quality.json", {
        "input_fingerprint": request.context.input_fingerprint,
        "image": image, "policy": config["quality_policy"], "measurements": measurements, "checks": checks,
    })
    return seal_result(request, directory, [dict(result, kind="CHECK_RESULT", media_type="application/json")])


def _inspect(inputs, policy):
    image = {"sha256": inputs.image.sha256, "width": None, "height": None}
    try:
        contents = verified_bytes(inputs.image, IMAGE_LIMIT)
        pixels, facts = decode_display_image(contents, max_pixels=inputs.max_pixels)
        with pixels:
            measured = measure_image_quality(pixels, policy)
    except ImageInspectionError as error:
        return image, None, _unreadable(error.code)
    except FileNotFoundError:
        return image, None, _unreadable("offline")
    except OSError:
        return image, None, _unreadable("file_io_failed")
    image.update(width=facts.width, height=facts.height)
    checks = [_check("image.decode", "COMPLETED", "PASS")]
    for check, attribute, unavailable in PIXEL_CHECKS:
        if getattr(measured, attribute) is None:
            checks.append(_check(check, "SKIPPED", "NOT_EVALUATED", unavailable))
        else:
            assessment = "FINDINGS" if check in measured.findings else "PASS"
            checks.append(_check(check, "COMPLETED", assessment))
    return image, asdict(measured), checks


def _unreadable(code):
    checks = [_check("image.decode", "FAILED", "NOT_EVALUATED", code)]
    checks.extend(_check(check, "SKIPPED", "NOT_EVALUATED", "image_unreadable")
                  for check, _, _ in PIXEL_CHECKS)
    return checks


def _check(check_id, execution_status, assessment, reason_code=None):
    return {"check_id": check_id, "execution_status": execution_status,
            "assessment": assessment, "reason_code": reason_code}
