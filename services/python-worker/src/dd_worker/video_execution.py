"""Write an authorized display-order observation for Core's video index."""

from dataclasses import asdict
import os
from pathlib import Path
from uuid import UUID

from video_decoder import VideoDecodeError, VideoDecoder
from video_tracking import TrackSeed, TranslationPolicy, track_indexed_video
from worker_execution import (
    InputChanged, seal_result, verified_manifest, verified_manifest_asset, verified_path, write_bytes, write_json,
)

CAPABILITY = "video_probe.v1"
PROBE_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0"
RESOLVE_CAPABILITY = "frame_resolve.v1"
RESOLVE_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartFrameResolve\0"
TRACK_CAPABILITY = "video_tracking.v1"
TRACK_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartTracking\0"
FAILURES = {
    "source_changed": "input_changed",
    "source_unavailable": "file_io_failed",
    "pixel_limit_exceeded": "pixel_limit_exceeded",
    "decode_failed": "decode_failed",
    "unsupported_rotation": "unsupported_format",
    "changing_frame_dimensions": "unsupported_format",
}


def configured_tools():
    ffmpeg = Path(os.environ.get("DD_WORKER_VIDEO_FFMPEG", ""))
    ffprobe = Path(os.environ.get("DD_WORKER_VIDEO_FFPROBE", ""))
    if all(tool.is_absolute() and tool.is_file() for tool in (ffmpeg, ffprobe)):
        return ffmpeg, ffprobe
    return None


def compute_probe(request, *, tools):
    return _compute(request, tools, _probe)


def compute_resolve(request, *, tools):
    return _compute(request, tools, _resolve)


def compute_tracking(request, *, tools):
    return _compute(request, tools, _track)


def _compute(request, tools, operation):
    try:
        return operation(request, tools), None
    except VideoDecodeError as error:
        return None, FAILURES.get(error.code, "invalid_input")
    except InputChanged:
        return None, "input_changed"
    except (ValueError, TypeError, KeyError, IndexError, RecursionError):
        return None, "invalid_input"
    except OSError:
        return None, "file_io_failed"


def _probe(request, tools):
    inputs = request.parameters
    source, video_id, decoder = _read_source(request, tools, verified_manifest_asset(request, CAPABILITY))
    selected = inputs.stream_index if inputs.HasField("stream_index") else None
    index = decoder.probe(source, inputs.source_video.sha256, stream_index=selected)
    directory = verified_path(inputs.output_directory)
    entry = write_json(directory, "video-index.json", {
        "source_video_id": str(video_id),
        "source_digest": index.source_sha256,
        "stream_index": index.stream_index,
        "time_base": index.time_base,
        "rotation_ccw": index.rotation_ccw,
        "metadata": {
            "duration_ms": index.duration_ms,
            "streams": [asdict(stream) for stream in index.streams],
            "variable_frame_rate": index.variable_frame_rate,
        },
        "frames": [
            {"raw_pts": None if frame.raw_pts is None else str(frame.raw_pts),
             "width": frame.width, "height": frame.height}
            for frame in index.frames
        ],
    })
    return seal_result(request, directory, [dict(entry, kind="VIDEO_FRAME_INDEX", media_type="application/json")])


def _resolve(request, tools):
    inputs = request.parameters
    if not inputs.HasField("frame_index"):
        raise ValueError("Frame location is required")
    source, video_id, decoder = _read_source(request, tools, verified_manifest_asset(request, RESOLVE_CAPABILITY))
    index = decoder.probe(source, inputs.source_video.sha256, stream_index=inputs.stream_index)
    offset = inputs.frame_index
    if offset >= len(index.frames):
        raise ValueError("Frame is outside the actual display index")
    frame = index.frames[offset]
    expected_pts = inputs.expected_raw_pts if inputs.HasField("expected_raw_pts") else None
    display_size = (frame.width, frame.height)
    if index.rotation_ccw in (90, 270):
        display_size = (frame.height, frame.width)
    if frame.raw_pts != expected_pts or display_size != (inputs.display_width, inputs.display_height):
        raise InputChanged("Frame no longer matches the indexed display metadata")
    png = decoder.decode_png(source, index, offset)
    directory = verified_path(inputs.output_directory)
    image = write_bytes(directory, "frame.png", png)
    details = write_json(directory, "frame.json", {
        "source_video_id": str(video_id),
        "source_digest": index.source_sha256,
        "stream_index": index.stream_index,
        "frame_index": str(offset),
        "raw_pts": None if frame.raw_pts is None else str(frame.raw_pts),
        "width": display_size[0],
        "height": display_size[1],
        "image_artifact_id": image["artifact_id"],
    })
    return seal_result(request, directory, [
        dict(image, kind="VIDEO_FRAME_IMAGE", media_type="image/png"),
        dict(details, kind="VIDEO_FRAME_REF", media_type="application/json"),
    ])


def _track(request, tools):
    parameters = request.parameters
    manifest = verified_manifest(request, TRACK_CAPABILITY)
    source, video_id, decoder = _read_source(request, tools, manifest["assets"][0])
    if (not 1 <= len(parameters.frames) <= 512 or not 1 <= len(parameters.seeds) <= 128
            or len(parameters.tail_anchors) > len(parameters.seeds)):
        raise ValueError("Invalid size of tracking selection or anchor lists")
    segment_id = _canonical_id(parameters.segment_id)
    profile_id = _canonical_id(parameters.tracker_profile_id)
    selected = _tracking_config(parameters, segment_id, profile_id)
    if selected != manifest["resolved_config"].get("tracking_binding"):
        raise InputChanged("Tracking request differs from its frozen manifest")
    index = decoder.probe(source, parameters.source_video.sha256, stream_index=parameters.stream_index)
    offsets = []
    for expected in parameters.frames:
        if not expected.HasField("frame_index") or expected.frame_index >= len(index.frames):
            raise ValueError("Tracking frame has no valid display index")
        observed = index.frames[expected.frame_index]
        dimensions = (observed.width, observed.height)
        if index.rotation_ccw in (90, 270):
            dimensions = dimensions[::-1]
        pts = expected.expected_raw_pts if expected.HasField("expected_raw_pts") else None
        if observed.raw_pts != pts or dimensions != (expected.display_width, expected.display_height):
            raise InputChanged("Tracking frame no longer matches its indexed display metadata")
        offsets.append(expected.frame_index)

    policy = TranslationPolicy(parameters.search_radius, parameters.max_error, parameters.min_margin)
    def seed(entry):
        return TrackSeed(entry.track_id, entry.category_id, tuple(entry.bbox_xywh))

    result = track_indexed_video(
        decoder, source, index, offsets, [seed(entry) for entry in parameters.seeds], policy,
        tail_anchors=[seed(entry) for entry in parameters.tail_anchors],
    )
    directory = verified_path(parameters.output_directory)
    entry = write_json(directory, "video-tracks.json", {
        "contract_version": 1,
        "source_video_id": str(video_id),
        "source_digest": index.source_sha256,
        "stream_index": index.stream_index,
        "segment_id": segment_id,
        "tracker_profile_id": profile_id,
        "algorithm": "translation_template.v1",
        "input_fingerprint": request.context.input_fingerprint,
        "processed_frame_count": result.processed_frame_count,
        "observations": [
            {"frame_index": str(item.frame_index), "track_id": item.track_id, "category_id": str(item.category_id),
             "bbox_xywh": item.bbox, "source": item.source, "mean_absolute_error": item.mean_absolute_error}
            for item in result.observations
        ],
        "uncertain_ranges": [
            {"track_id": span.track_id, "first_frame_index": str(span.first_frame_index),
             "last_frame_index": str(span.last_frame_index), "reason": span.reason}
            for span in result.uncertain_ranges
        ],
        "tail_checks": [asdict(check) for check in result.tail_checks],
    })
    return seal_result(request, directory, [dict(entry, kind="VIDEO_TRACK_BUNDLE", media_type="application/json")])


def _tracking_config(parameters, segment_id, profile_id):
    def anchors(entries):
        return [{"track_id": entry.track_id, "category_id": str(entry.category_id), "bbox_xywh": list(entry.bbox_xywh)}
                for entry in entries]

    return {
        "segment_id": segment_id,
        "tracker_profile_id": profile_id,
        "stream_index": parameters.stream_index,
        "frames": [
            {"frame_index": str(frame.frame_index),
             "raw_pts": str(frame.expected_raw_pts) if frame.HasField("expected_raw_pts") else None,
             "width": frame.display_width, "height": frame.display_height}
            for frame in parameters.frames
        ],
        "seeds": anchors(parameters.seeds),
        "tail_anchors": anchors(parameters.tail_anchors),
        "policy": {"search_radius": parameters.search_radius,
                   "max_error": parameters.max_error, "min_margin": parameters.min_margin},
        "limits": {"max_pixels": str(parameters.max_pixels), "max_frames": parameters.max_frames,
                   "timeout_ms": parameters.timeout_ms},
    }


def _canonical_id(value):
    identifier = UUID(value)
    if identifier.int == 0 or str(identifier) != value:
        raise ValueError("Invalid tracking identity")
    return value


def _read_source(request, tools, expected):
    inputs = request.parameters
    video_id = UUID(expected["asset_id"])
    if (
        expected["media_digest"] != inputs.source_video.sha256
        or not 1 <= inputs.max_pixels <= 100_000_000
        or not 1 <= inputs.max_frames <= 100_000
        or not 1 <= inputs.timeout_ms <= 120_000
    ):
        raise ValueError("Video source or limits do not match the grant")
    decoder = VideoDecoder(*tools, max_pixels=inputs.max_pixels, max_frames=inputs.max_frames,
                           timeout_seconds=inputs.timeout_ms / 1000)
    return verified_path(inputs.source_video.path), video_id, decoder
