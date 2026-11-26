"""Index and decode local video frames with a configured FFmpeg executable pair."""

from dataclasses import dataclass
from hashlib import file_digest
import json
from math import isfinite
from pathlib import Path
import subprocess
from threading import Event, Timer

from dd_worker.image_inspection import ImageInspectionError, inspect_image


INPUT_OPTIONS = (
    "-protocol_whitelist", "file",
    "-format_whitelist", "mov,matroska,webm,avi,mpegts",
    "-threads", "1",
)
PROBE_BYTE_LIMIT = 32 * 1024 * 1024


class VideoDecodeError(Exception):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


@dataclass(frozen=True)
class DisplayFrame:
    raw_pts: int | None
    width: int
    height: int
    picture_type: str | None


@dataclass(frozen=True)
class VideoIndex:
    source_sha256: str
    stream_index: int
    time_base: tuple[int, int] | None
    rotation_ccw: int
    frames: tuple[DisplayFrame, ...]


class VideoDecoder:
    def __init__(
        self,
        ffmpeg: Path,
        ffprobe: Path,
        *,
        max_pixels: int = 16_000_000,
        max_frames: int = 100_000,
        timeout_seconds: float = 120,
    ):
        if min(max_pixels, max_frames, timeout_seconds) <= 0 or not isfinite(timeout_seconds):
            raise ValueError("Video decoding limits must be finite and positive")
        self.ffmpeg = ffmpeg.resolve(strict=True)
        self.ffprobe = ffprobe.resolve(strict=True)
        self.max_pixels = max_pixels
        self.max_frames = max_frames
        self.timeout_seconds = timeout_seconds

    def probe(self, source: Path, expected_sha256: str) -> VideoIndex:
        source = _verified_source(source, expected_sha256)
        output = self._capture(
            self.ffprobe,
            [
                *INPUT_OPTIONS, "-select_streams", "V:0", "-show_streams", "-show_frames",
                "-show_entries",
                "stream=index,time_base:stream_side_data=rotation:frame=pts,width,height,pict_type",
                "-of", "json", "-i", str(source),
            ],
            PROBE_BYTE_LIMIT,
        )
        _verified_source(source, expected_sha256)
        try:
            metadata = json.loads(output)
            stream, = metadata["streams"]
            raw_frames = metadata["frames"]
            if not raw_frames:
                raise VideoDecodeError("no_display_frames")
            if len(raw_frames) > self.max_frames:
                raise VideoDecodeError("frame_limit_exceeded")
            rotations = [item["rotation"] for item in stream.get("side_data_list", [])
                         if "rotation" in item]
            rotation, = rotations or [0]
            time_base = _read_time_base(stream.get("time_base"))
            frames = tuple(self._frame(frame) for frame in raw_frames)
            if any((frame.width, frame.height) != (frames[0].width, frames[0].height)
                   for frame in frames):
                raise VideoDecodeError("changing_frame_dimensions")
            return VideoIndex(expected_sha256, stream["index"], time_base,
                              _right_angle(rotation), frames)
        except (KeyError, TypeError, ValueError) as error:
            raise VideoDecodeError("decode_failed") from error

    def decode_png(self, source: Path, index: VideoIndex, frame_index: int) -> bytes:
        if type(frame_index) is not int or not 0 <= frame_index < len(index.frames):
            raise VideoDecodeError("invalid_frame")
        source = _verified_source(source, index.source_sha256)
        frame = index.frames[frame_index]
        if frame.width * frame.height > self.max_pixels:
            raise VideoDecodeError("pixel_limit_exceeded")
        png = self._capture(
            self.ffmpeg,
            [
                "-nostdin", "-xerror", "-filter_threads", "1", *INPUT_OPTIONS,
                "-reinit_filter:v", "0", "-i", str(source),
                "-map", f"0:{index.stream_index}", "-vf", f"select=eq(n\\,{frame_index})",
                "-fps_mode", "passthrough", "-frames:v", "1", "-c:v", "png", "-threads", "1",
                "-f", "image2pipe", "pipe:1",
            ],
            4 * frame.width * frame.height + 1024 * 1024,
        )
        _verified_source(source, index.source_sha256)
        width, height = frame.width, frame.height
        if index.rotation_ccw in (90, 270):
            width, height = height, width
        try:
            image = inspect_image(png, max_pixels=self.max_pixels)
        except ImageInspectionError as error:
            raise VideoDecodeError(error.code) from error
        if (image.width, image.height, image.media_type) != (width, height, "image/png"):
            raise VideoDecodeError("unexpected_display_frame")
        return png

    def _frame(self, frame: dict) -> DisplayFrame:
        width, height = frame["width"], frame["height"]
        if width <= 0 or height <= 0:
            raise VideoDecodeError("decode_failed")
        if width * height > self.max_pixels:
            raise VideoDecodeError("pixel_limit_exceeded")
        return DisplayFrame(frame.get("pts"), width, height, frame.get("pict_type"))

    def _capture(self, executable: Path, args: list[str], limit: int) -> bytes:
        try:
            with subprocess.Popen(
                [str(executable), "-hide_banner", "-loglevel", "error", *args],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            ) as process:
                expired = Event()

                def stop_on_timeout():
                    if process.poll() is None:
                        expired.set()
                        process.kill()

                timer = Timer(self.timeout_seconds, stop_on_timeout)
                timer.start()
                try:
                    output = process.stdout.read(limit + 1)
                    if len(output) > limit:
                        process.kill()
                        raise VideoDecodeError("output_limit_exceeded")
                    code = process.wait()
                    if expired.is_set():
                        raise VideoDecodeError("decode_timed_out")
                    if code != 0 or not output:
                        raise VideoDecodeError("decode_failed")
                    return output
                finally:
                    timer.cancel()
                    timer.join()
                    if process.poll() is None:
                        process.kill()
        except OSError as error:
            raise VideoDecodeError("decode_failed") from error


def _verified_source(source: Path, expected_sha256: str) -> Path:
    try:
        source = source.resolve(strict=True)
        if not source.is_file():
            raise VideoDecodeError("source_unavailable")
        with source.open("rb") as contents:
            if file_digest(contents, "sha256").hexdigest() != expected_sha256:
                raise VideoDecodeError("source_changed")
    except OSError as error:
        raise VideoDecodeError("source_unavailable") from error
    return source


def _read_time_base(value: str | None) -> tuple[int, int] | None:
    if value is None:
        return None
    numerator, denominator = map(int, value.split("/"))
    return (numerator, denominator) if min(numerator, denominator) > 0 else None


def _right_angle(rotation: int) -> int:
    if rotation % 90 != 0:
        raise VideoDecodeError("unsupported_rotation")
    return rotation % 360
