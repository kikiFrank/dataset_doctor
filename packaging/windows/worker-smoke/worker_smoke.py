"""Private stdin/stdout smoke worker for the Windows onedir packaging check."""

import json
import sys
import sysconfig

from PIL import Image, PngImagePlugin, __version__ as pillow_version, features


def receive(operation):
    message = json.loads(sys.stdin.readline())
    if not isinstance(message, dict) or message.get("op") != operation:
        raise ValueError(f"expected {operation}")
    return message


def send(message):
    print(json.dumps(message, ensure_ascii=True), flush=True)


def main():
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

    try:
        receive("hello")
        Image.init()
        if PngImagePlugin.PngImageFile.format != "PNG" or not features.check_codec("zlib"):
            raise RuntimeError("Pillow PNG decoding is unavailable")
        send(
            {
                "type": "ready",
                "worker": "dataset-doctor-p00-02",
                "python": sys.version.split()[0],
                "platform": sysconfig.get_platform(),
                "pillow": pillow_version,
                "capabilities": ["cpu-png"],
            }
        )

        request = receive("decode")
        path = request.get("path")
        if not isinstance(path, str) or not path:
            raise ValueError("decode requires an image path")

        try:
            with Image.open(path, formats=["PNG"]) as image:
                image.load()
                if image.mode != "RGB":
                    raise ValueError("expected an RGB fixture")
                width, height = image.size
                pixels = [
                    list(image.getpixel((x, y)))
                    for y in range(height)
                    for x in range(width)
                ]
        except (OSError, ValueError) as error:
            print(f"PNG decode failed: {error}", file=sys.stderr, flush=True)
            send({"type": "error", "code": "decode_failed"})
            return 2

        send(
            {
                "type": "decoded",
                "width": width,
                "height": height,
                "mode": "RGB",
                "pixels": pixels,
            }
        )
        receive("shutdown")
        send({"type": "bye"})
        return 0
    except (OSError, RuntimeError, ValueError) as error:
        print(f"Worker smoke failed: {error}", file=sys.stderr, flush=True)
        return 3


if __name__ == "__main__":
    raise SystemExit(main())
