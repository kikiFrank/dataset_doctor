"""Write an authorized display-order observation for Core's video index."""

import os
from pathlib import Path
from uuid import UUID

from video_decoder import VideoDecodeError, VideoDecoder
from worker_execution import (
    InputChanged, seal_result, verified_manifest_asset, verified_path, write_bytes, write_json,
)

CAPABILITY = "video_probe.v1"
PROBE_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0"
RESOLVE_CAPABILITY = "frame_resolve.v1"
RESOLVE_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartFrameResolve\0"
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
    source, video_id, decoder = _read_source(request, tools, CAPABILITY)
    selected = inputs.stream_index if inputs.HasField("stream_index") else None
    index = decoder.probe(source, inputs.source_video.sha256, stream_index=selected)
    directory = verified_path(inputs.output_directory)
    entry = write_json(directory, "video-index.json", {
        "source_video_id": str(video_id),
        "source_digest": index.source_sha256,
        "stream_index": index.stream_index,
        "time_base": index.time_base,
        "rotation_ccw": index.rotation_ccw,
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
    source, video_id, decoder = _read_source(request, tools, RESOLVE_CAPABILITY)
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


def _read_source(request, tools, capability):
    inputs = request.parameters
    expected = verified_manifest_asset(request, capability)
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
