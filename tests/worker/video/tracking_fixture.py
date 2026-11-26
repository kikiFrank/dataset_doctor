"""Generate a deterministic textured target and a synthetic lossless video."""

from pathlib import Path
import subprocess
import sys

from PIL import Image


def pattern_frame(*positions, size=(34, 26)):
    image = Image.new("RGB", size, (16, 16, 16))
    with Image.new("RGB", (5, 5)) as patch:
        levels = [71, 204, 111, 249, 169, 251, 92, 211, 117, 191, 122, 176, 240,
                  155, 82, 184, 219, 99, 224, 142, 230, 114, 160, 197, 232]
        patch.putdata([(level, level, level) for level in levels])
        for origin in positions:
            image.paste(patch, origin)
    return image


def make_tracking_clip(output: Path, ffmpeg: Path) -> None:
    frames = output.with_suffix(".frames")
    frames.mkdir()
    for index, position in enumerate(((8, 8), None, (11, 9), None, (15, 11), None)):
        with pattern_frame(*((position,) if position else ())) as image:
            image.save(frames / f"frame-{index:02d}.png")
    subprocess.run(
        [str(ffmpeg), "-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1",
         "-framerate", "10", "-i", str(frames / "frame-%02d.png"), "-c:v", "ffv1",
         "-pix_fmt", "bgr0", str(output)],
        stdin=subprocess.DEVNULL, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        timeout=20, check=True,
    )


if __name__ == "__main__":
    make_tracking_clip(Path(sys.argv[1]), Path(sys.argv[2]).resolve(strict=True))
