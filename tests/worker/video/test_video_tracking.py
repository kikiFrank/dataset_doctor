"""Exercise the translation tracker with real pixel buffers and known motion."""

from hashlib import sha256
import os
from pathlib import Path
import sys
import tempfile
import unittest

from tracking_fixture import make_tracking_clip, pattern_frame

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "services/python-worker/src"))

from dd_worker.video_decoder import VideoDecodeError, VideoDecoder
from dd_worker.video_tracking import (
    TrackingFrame,
    TrackSeed,
    TranslationPolicy,
    track_indexed_video,
    track_translation,
)


TARGET = "7331d90c-7bab-4d29-9760-0819fb9b023a"
OTHER = "0a4ca430-165b-40ca-a70e-3043cd87d16d"
SEED = TrackSeed(TARGET, 3, (8, 8, 5, 5))
POLICY = TranslationPolicy(search_radius=6, max_error=0.06, min_margin=0.01)


def frame(index, *positions, size=(34, 26)):
    return TrackingFrame(index, pattern_frame(*positions, size=size))


class TranslationTrackingTest(unittest.TestCase):
    def test_consumes_exact_decoded_frames_from_a_verified_video(self):
        run = Path(os.environ["DD_TEST_VIDEO_RUN"]).resolve(strict=True)
        folder = Path(tempfile.mkdtemp(prefix="tracking-clip-", dir=run))
        ffmpeg = Path(os.environ["DD_TEST_FFMPEG"]).resolve(strict=True)
        ffprobe = Path(os.environ["DD_TEST_FFPROBE"]).resolve(strict=True)
        clip = folder / "Tracked pattern.mkv"
        make_tracking_clip(clip, ffmpeg)
        original = clip.read_bytes()
        decoder = VideoDecoder(ffmpeg, ffprobe)
        index = decoder.probe(clip, sha256(original).hexdigest())
        result = track_indexed_video(decoder, clip, index, [0, 2, 4, 5], [SEED], POLICY)
        self.assertEqual(result.processed_frame_count, 4)
        self.assertEqual([(box.frame_index, box.bbox) for box in result.observations],
                         [(0, (8, 8, 5, 5)), (2, (11, 9, 5, 5)), (4, (15, 11, 5, 5))])
        self.assertEqual(result.uncertain_ranges[0].first_frame_index, 5)
        self.assertEqual(result.uncertain_ranges[0].reason, "TARGET_LOST")
        self.assertEqual(clip.read_bytes(), original)
        with self.assertRaises(ValueError):
            track_indexed_video(decoder, clip, index, [0, 2, 2], [SEED], POLICY)
        with self.assertRaises(ValueError):
            track_indexed_video(decoder, clip, index, [0, 7], [SEED], POLICY)
        clip.write_bytes(original + b"changed")
        with self.assertRaises(VideoDecodeError) as error:
            track_indexed_video(decoder, clip, index, [0, 2], [SEED], POLICY)
        self.assertEqual(error.exception.code, "source_changed")

    def test_tracks_actual_translation_with_stable_identity_and_category(self):
        frames = [frame(2, (8, 8)), frame(5, (11, 9)), frame(11, (15, 11))]
        originals = [sample.image.tobytes() for sample in frames]
        result = track_translation(iter(frames), [SEED], POLICY)
        self.assertEqual(result.processed_frame_count, 3)
        self.assertFalse(result.uncertain_ranges)
        self.assertEqual(
            [(box.frame_index, box.track_id, box.category_id, box.bbox, box.source)
             for box in result.observations],
            [(2, TARGET, 3, (8, 8, 5, 5), "HUMAN_SEED"),
             (5, TARGET, 3, (11, 9, 5, 5), "TRACKER"),
             (11, TARGET, 3, (15, 11, 5, 5), "TRACKER")],
        )
        self.assertIsNone(result.observations[0].mean_absolute_error)
        self.assertEqual(result.observations[2].mean_absolute_error, 0.0)
        self.assertEqual([sample.image.tobytes() for sample in frames], originals)

    def test_does_not_choose_between_equal_targets_or_reacquire_a_lost_identity(self):
        ambiguous = track_translation(
            [frame(0, (8, 8)), frame(1, (2, 8), (13, 9)), frame(8, (10, 8))],
            [SEED], POLICY,
        )
        self.assertEqual([box.frame_index for box in ambiguous.observations], [0])
        self.assertEqual(
            [(span.track_id, span.first_frame_index, span.last_frame_index, span.reason)
             for span in ambiguous.uncertain_ranges],
            [(TARGET, 1, 8, "AMBIGUOUS_MATCH")],
        )
        lost = track_translation([frame(0, (8, 8)), frame(3), frame(5, (9, 8))], [SEED], POLICY)
        self.assertEqual([box.frame_index for box in lost.observations], [0])
        self.assertEqual(lost.uncertain_ranges[0].reason, "TARGET_LOST")
        self.assertEqual(lost.uncertain_ranges[0].last_frame_index, 5)

    def test_two_tracks_cannot_claim_the_same_match(self):
        other = TrackSeed(OTHER, 3, (18, 8, 5, 5))
        result = track_translation(
            [frame(0, (8, 8), (18, 8)), frame(1, (13, 8)), frame(3, (9, 8), (19, 8))],
            [SEED, other], POLICY,
        )
        self.assertEqual([box.frame_index for box in result.observations], [0, 0])
        self.assertEqual(
            [(span.track_id, span.first_frame_index, span.last_frame_index, span.reason)
             for span in result.uncertain_ranges],
            [(TARGET, 1, 3, "AMBIGUOUS_IDENTITY"), (OTHER, 1, 3, "AMBIGUOUS_IDENTITY")],
        )

    def test_checks_a_tail_anchor_without_replacing_the_computed_candidate(self):
        frames = [frame(2, (8, 8)), frame(5, (11, 9))]
        contrary = TrackSeed(TARGET, 3, (12, 9, 5, 5))
        result = track_translation(frames, [SEED], POLICY, tail_anchors=[contrary])
        self.assertEqual(result.observations[-1].bbox, (11, 9, 5, 5))
        self.assertEqual([(check.track_id, check.status) for check in result.tail_checks],
                         [(TARGET, "DISAGREES")])
        actual = TrackSeed(TARGET, 3, (11, 9, 5, 5))
        result = track_translation(frames, [SEED], POLICY, tail_anchors=[actual])
        self.assertEqual(result.tail_checks[0].status, "MATCHES")

    def test_flat_seeds_are_uncertain_and_invalid_bindings_are_rejected(self):
        result = track_translation([frame(0), frame(2, (8, 8))], [SEED], POLICY)
        self.assertEqual([box.frame_index for box in result.observations], [0])
        self.assertEqual(result.uncertain_ranges[0].reason, "UNSUPPORTED_SEED")
        self.assertEqual(result.uncertain_ranges[0].first_frame_index, 2)
        self.assertRaises(ValueError, track_translation, [], [SEED], POLICY)
        self.assertRaises(ValueError, track_translation, [frame(1), frame(1)], [SEED], POLICY)
        self.assertRaises(ValueError, track_translation, [frame(0), frame(1, size=(20, 20))], [SEED], POLICY)
        self.assertRaises(ValueError, track_translation, [frame(0)], [SEED, SEED], POLICY)
        self.assertRaises(ValueError, track_translation, [frame(0)], [TrackSeed(OTHER, 3, (33, 8, 5, 5))], POLICY)
        self.assertRaises(ValueError, track_translation, [frame(0)], [SEED],
                          TranslationPolicy(search_radius=0, max_error=0.06, min_margin=0.01))


if __name__ == "__main__":
    unittest.main()
