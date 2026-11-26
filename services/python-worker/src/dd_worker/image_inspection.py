"""Decode the exact image bytes selected by Core for an inspection."""

from dataclasses import dataclass
from hashlib import sha256
from io import BytesIO

from PIL import Image, ImageOps

MEDIA_TYPES = {"PNG": "image/png", "JPEG": "image/jpeg"}


class ImageInspectionError(ValueError):
    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


@dataclass(frozen=True, slots=True)
class InspectedImage:
    width: int
    height: int
    media_type: str
    sha256: str


def inspect_image(encoded: bytes, *, max_pixels: int) -> InspectedImage:
    if max_pixels <= 0:
        raise ValueError("max_pixels must be positive")
    try:
        with Image.open(BytesIO(encoded)) as source:
            media_type = MEDIA_TYPES.get(source.format)
            if media_type is None:
                raise ImageInspectionError("unsupported_format")
            if source.width * source.height > max_pixels:
                raise ImageInspectionError("pixel_limit_exceeded")
            source.load()
            with ImageOps.exif_transpose(source) as display:
                return InspectedImage(display.width, display.height, media_type, sha256(encoded).hexdigest())
    except ImageInspectionError:
        raise
    except Image.DecompressionBombError:
        raise ImageInspectionError("pixel_limit_exceeded") from None
    except (OSError, ValueError, SyntaxError):
        raise ImageInspectionError("decode_failed") from None
