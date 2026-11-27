"""Cancel a real FFmpeg process blocked producing no output."""

import os
from pathlib import Path
import sys
from threading import Event, Timer
from time import monotonic
import unittest

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src/dd_worker"))
from execution_control import ExecutionCancelled, execution_scope
from video_decoder import VideoDecoder


class VideoCancellationTests(unittest.TestCase):
    def test_cancel_reaps_the_actual_ffmpeg_before_its_decode_timeout(self):
        decoder = VideoDecoder(Path(os.environ["DD_TEST_FFMPEG"]), Path(os.environ["DD_TEST_FFPROBE"]), timeout_seconds=2)
        cancellation = Event()
        timer = Timer(0.2, cancellation.set)
        start = monotonic()
        timer.start()
        try:
            with execution_scope(cancellation), self.assertRaises(ExecutionCancelled):
                decoder._capture(decoder.ffmpeg, [
                    "-nostdin", "-re", "-f", "lavfi", "-i", "testsrc=size=16x16:rate=1",
                    "-f", "null", "-",
                ], 1024)
        finally:
            timer.cancel()
            timer.join()
        self.assertLess(monotonic() - start, 1.5)


if __name__ == "__main__":
    unittest.main()
