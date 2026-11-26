"""Decode synthetic clips with the same executable pair used by the adapter."""

from hashlib import file_digest
from io import BytesIO
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from PIL import Image

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))


FFMPEG = Path(os.environ["DD_TEST_FFMPEG"]).resolve(strict=True)
FFPROBE = Path(os.environ["DD_TEST_FFPROBE"]).resolve(strict=True)
RUN = Path(os.environ["DD_TEST_VIDEO_RUN"]).resolve(strict=True)
RED, GREEN, BLUE = (235, 25, 30), (25, 235, 30), (25, 30, 235)


def ffmpeg(*args):
    subprocess.run(
        [str(FFMPEG), "-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1", *args],
        stdin=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        timeout=20,
        check=True,
    )


def digest(path):
    with path.open("rb") as source:
        return file_digest(source, "sha256").hexdigest()


class VideoDecoderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.folder = Path(tempfile.mkdtemp(prefix="clips-", dir=RUN))
        for index, color in enumerate((RED, GREEN, BLUE) * 3):
            with Image.new("RGB", (48, 32), color) as image:
                image.save(cls.folder / f"frame-{index:02d}.png")
        cls.cfr = cls.folder / "b-frames.mp4"
        ffmpeg(
            "-framerate", "10", "-i", str(cls.folder / "frame-%02d.png"),
            "-c:v", "mpeg4", "-bf", "2", "-q:v", "2", "-pix_fmt", "yuv420p",
            "-output_ts_offset", "2", str(cls.cfr),
        )
        manifest = cls.folder / "variable.ffconcat"
        manifest.write_text(
            "ffconcat version 1.0\nfile frame-00.png\nduration 0.08\n"
            "file frame-01.png\nduration 0.20\nfile frame-02.png\n",
            encoding="ascii",
        )
        cls.vfr = cls.folder / "variable.mkv"
        ffmpeg("-f", "concat", "-safe", "1", "-i", str(manifest),
               "-fps_mode", "passthrough", "-c:v", "ffv1", str(cls.vfr))
        pattern = cls.folder / "pattern.png"
        with Image.new("RGB", (48, 32)) as image:
            image.paste(RED, (0, 0, 24, 16))
            image.paste(GREEN, (24, 0, 48, 16))
            image.paste(BLUE, (0, 16, 24, 32))
            image.save(pattern)
        unrotated = cls.folder / "pattern.mp4"
        ffmpeg("-i", str(pattern), "-c:v", "mpeg4", "-q:v", "2",
               "-pix_fmt", "yuv420p", str(unrotated))
        cls.rotated = cls.folder / "rotated.mp4"
        ffmpeg("-display_rotation:v:0", "90", "-i", str(unrotated), "-c", "copy", str(cls.rotated))

    def decoder(self, **options):
        from dd_worker.video_decoder import VideoDecoder

        return VideoDecoder(FFMPEG, FFPROBE, **options)

    def assert_color(self, image, point, expected):
        actual = image.getpixel(point)
        self.assertTrue(all(abs(a - b) < 12 for a, b in zip(actual, expected)), (actual, expected))

    def test_b_frames_use_display_order_and_preserve_nonzero_source_pts(self):
        decoder = self.decoder()
        index = decoder.probe(self.cfr, digest(self.cfr))
        self.assertEqual(len(index.frames), 9)
        self.assertIn("B", [frame.picture_type for frame in index.frames])
        self.assertEqual(index.frames[0].raw_pts * index.time_base[0], 2 * index.time_base[1])
        self.assertEqual(len(set(frame.raw_pts for frame in index.frames)), 9)
        for frame_index, color in enumerate((RED, GREEN, BLUE) * 3):
            with Image.open(BytesIO(decoder.decode_png(self.cfr, index, frame_index))) as frame:
                self.assertEqual(frame.size, (48, 32))
                self.assert_color(frame.convert("RGB"), (22, 15), color)

    def test_vfr_uses_original_timestamps_without_repeating_frames(self):
        decoder = self.decoder()
        index = decoder.probe(self.vfr, digest(self.vfr))
        ticks = [frame.raw_pts for frame in index.frames]
        self.assertEqual(len(ticks), 3)
        numerator, denominator = index.time_base
        self.assertEqual([(tick - ticks[0]) * numerator * 1000 // denominator for tick in ticks],
                         [0, 80, 280])
        with Image.open(BytesIO(decoder.decode_png(self.vfr, index, 2))) as frame:
            self.assert_color(frame.convert("RGB"), (22, 15), BLUE)

    def test_multistream_container_requires_an_explicit_video_stream(self):
        from dd_worker.video_decoder import VideoDecodeError

        multistream = self.folder / "multiple-video-streams.mp4"
        ffmpeg(
            "-f", "lavfi", "-i", "color=blue:s=48x32:r=10:d=0.4",
            "-f", "lavfi", "-i", "color=red:s=48x32:r=10:d=0.6",
            "-map", "0:v", "-map", "1:v", "-c:v", "mpeg4", str(multistream),
        )
        decoder = self.decoder()
        with self.assertRaises(VideoDecodeError) as error:
            decoder.probe(multistream, digest(multistream))
        self.assertEqual(error.exception.code, "video_stream_selection_required")
        index = decoder.probe(multistream, digest(multistream), stream_index=1)
        self.assertEqual(index.stream_index, 1)
        self.assertEqual(len(index.frames), 6)
        with Image.open(BytesIO(decoder.decode_png(multistream, index, 0))) as frame:
            red, green, blue = frame.convert("RGB").getpixel((22, 15))
            self.assertTrue(red > 240 and green < 12 and blue < 12)
        with self.assertRaises(VideoDecodeError) as error:
            decoder.probe(multistream, digest(multistream), stream_index=2)
        self.assertEqual(error.exception.code, "invalid_video_stream")

    def test_rotation_changes_display_dimensions_and_pixels(self):
        decoder = self.decoder()
        index = decoder.probe(self.rotated, digest(self.rotated))
        self.assertEqual(index.rotation_ccw, 90)
        with Image.open(BytesIO(decoder.decode_png(self.rotated, index, 0))) as frame:
            self.assertEqual(frame.size, (32, 48))
            with frame.convert("RGB") as rgb:
                self.assert_color(rgb, (8, 12), GREEN)
                self.assert_color(rgb, (8, 34), RED)
                self.assert_color(rgb, (24, 34), BLUE)

    def test_rejects_changed_source_invalid_frame_and_decoding_limits(self):
        from dd_worker.video_decoder import VideoDecodeError

        decoder = self.decoder()
        index = decoder.probe(self.cfr, digest(self.cfr))
        with self.assertRaises(VideoDecodeError) as error:
            decoder.decode_png(self.vfr, index, 0)
        self.assertEqual(error.exception.code, "source_changed")
        with self.assertRaises(VideoDecodeError) as error:
            decoder.decode_png(self.cfr, index, len(index.frames))
        self.assertEqual(error.exception.code, "invalid_frame")
        with self.assertRaises(VideoDecodeError) as error:
            self.decoder(max_frames=2).probe(self.cfr, digest(self.cfr))
        self.assertEqual(error.exception.code, "frame_limit_exceeded")
        with self.assertRaises(VideoDecodeError) as error:
            self.decoder(max_pixels=2).probe(self.cfr, digest(self.cfr))
        self.assertEqual(error.exception.code, "pixel_limit_exceeded")

    def test_rejects_playlist_with_external_entries_and_unreadable_data(self):
        from dd_worker.video_decoder import VideoDecodeError

        playlist = self.folder / "not-a-video.m3u8"
        playlist.write_text("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\n"
                            "https://127.0.0.1:1/unrequested.ts\n#EXT-X-ENDLIST\n", encoding="ascii")
        with self.assertRaises(VideoDecodeError) as error:
            self.decoder().probe(playlist, digest(playlist))
        self.assertEqual(error.exception.code, "decode_failed")

    def test_rejects_stream_whose_display_size_changes_mid_decode(self):
        from dd_worker.video_decoder import VideoDecodeError

        segments = []
        for width in (48, 64):
            segment = self.folder / f"segment-{width}.ts"
            ffmpeg("-f", "lavfi", "-i", f"color=red:s={width}x32:r=10:d=1",
                   "-c:v", "mpeg2video", "-g", "1", "-f", "mpegts", str(segment))
            segments.append(segment)
        changed_size = self.folder / "changed-size.ts"
        changed_size.write_bytes(b"".join(segment.read_bytes() for segment in segments))
        with self.assertRaises(VideoDecodeError) as error:
            self.decoder().probe(changed_size, digest(changed_size))
        self.assertEqual(error.exception.code, "changing_frame_dimensions")


if __name__ == "__main__":
    unittest.main(testRunner=unittest.TextTestRunner(stream=sys.stdout, verbosity=2))
