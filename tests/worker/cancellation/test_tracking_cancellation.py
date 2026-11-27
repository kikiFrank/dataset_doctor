"""Stop tracking between actual pixel frames without returning a partial result."""
from pathlib import Path
import sys
from threading import Event
import unittest

from PIL import Image

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src/dd_worker"))
from execution_control import ExecutionCancelled, execution_scope
from video_tracking import TrackingFrame, TrackSeed, TranslationPolicy, track_translation


class TrackingCancellationTests(unittest.TestCase):
    def test_a_stop_between_decoded_frames_does_not_complete_the_track(self):
        cancellation = Event()
        with Image.new("RGB", (16, 16)) as image:
            image.putpixel((5, 5), (255, 255, 255))
            original = image.tobytes()
            def frames():
                yield TrackingFrame(0, image)
                cancellation.set()
                yield TrackingFrame(1, image)
            with execution_scope(cancellation), self.assertRaises(ExecutionCancelled):
                track_translation(frames(), [TrackSeed("7331d90c-7bab-4d29-9760-0819fb9b023a", 1, (4, 4, 4, 4))], TranslationPolicy(search_radius=1))
            self.assertEqual(image.tobytes(), original)


if __name__ == "__main__":
    unittest.main()
