"""Write an authorized display-order observation for Core's video index."""

import os
from pathlib import Path
from uuid import UUID

from video_decoder import VideoDecodeError, VideoDecoder
from worker_execution import InputChanged, seal_result, verified_manifest_asset, verified_path, write_json

CAPABILITY = "video_probe.v1"
PROBE_DOMAIN = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0"
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
    try:
        return _probe(request, tools), None
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
    expected = verified_manifest_asset(request, CAPABILITY)
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
    source = verified_path(inputs.source_video.path)
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
