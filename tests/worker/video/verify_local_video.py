"""Verify an explicitly selected private local video through the production decoder."""

import argparse
from hashlib import file_digest, sha256
from io import BytesIO
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))

from PIL import Image
from dd_worker.video_decoder import VideoDecoder


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--video", required=True, type=Path)
    parser.add_argument("--ffmpeg", required=True, type=Path)
    parser.add_argument("--ffprobe", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--stream-index", type=int)
    args = parser.parse_args()
    private_root = (REPO / "target/local-private").resolve(strict=True)
    video = args.video.resolve(strict=True)
    output = args.output.resolve()
    if not video.is_relative_to(private_root) or not output.is_relative_to(private_root):
        parser.error("Private local validation uses a copied video and outputs under target/local-private.")
    output.mkdir(parents=True, exist_ok=False)
    with video.open("rb") as source:
        original_digest = file_digest(source, "sha256").hexdigest()
    decoder = VideoDecoder(args.ffmpeg, args.ffprobe, max_pixels=100_000_000,
                           max_frames=100_000, timeout_seconds=120)
    index = decoder.probe(video, original_digest, stream_index=args.stream_index)
    assert index.frames
    observations = []
    for offset in sorted({0, len(index.frames) // 2, len(index.frames) - 1}):
        png = decoder.decode_png(video, index, offset)
        expected = index.frames[offset]
        dimensions = (expected.width, expected.height)
        if index.rotation_ccw in (90, 270):
            dimensions = dimensions[::-1]
        with Image.open(BytesIO(png)) as image:
            image.load()
            assert image.format == "PNG"
            assert image.size == dimensions
        name = f"frame-{offset}.png"
        (output / name).write_bytes(png)
        observations.append({"frame_index": offset, "raw_pts": expected.raw_pts,
                             "width": dimensions[0], "height": dimensions[1],
                             "png_sha256": sha256(png).hexdigest(), "file": name})
    with video.open("rb") as source:
        assert file_digest(source, "sha256").hexdigest() == original_digest
    evidence = {"source_sha256": original_digest, "duration_ms": index.duration_ms,
                "stream_count": len(index.streams), "selected_stream": index.stream_index,
                "frame_count": len(index.frames), "rotation_ccw": index.rotation_ccw,
                "variable_frame_rate": index.variable_frame_rate, "frames": observations,
                "source_preserved": True}
    (output / "evidence.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in evidence.items() if k not in ("frames", "source_sha256")}))
    print("Private local evidence saved.")


if __name__ == "__main__":
    main()
