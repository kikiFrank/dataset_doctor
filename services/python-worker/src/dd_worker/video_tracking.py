"""Track translation around a human seed using actual normalized video pixels."""

from collections.abc import Iterable, Iterator, Sequence
from contextlib import closing
from dataclasses import dataclass
from io import BytesIO
from math import ceil, floor, inf, isfinite
from pathlib import Path
from uuid import UUID

from PIL import Image, ImageChops

if __package__:
    from .video_decoder import VideoDecoder, VideoIndex
    from .execution_control import checkpoint
else:
    from video_decoder import VideoDecoder, VideoIndex
    from execution_control import checkpoint


type Box = tuple[float, float, float, float]
_MAX_TEMPLATE_PIXELS = 65_536
_MAX_RADIUS = 24
_MAX_FRAME_INDEX = (1 << 64) - 1


@dataclass(frozen=True, slots=True)
class TrackingFrame:
    frame_index: int
    image: Image.Image


@dataclass(frozen=True, slots=True)
class TrackSeed:
    track_id: str
    category_id: int
    bbox: Box


@dataclass(frozen=True, slots=True)
class TranslationPolicy:
    search_radius: int = 8
    max_error: float = 0.08
    min_margin: float = 0.01


@dataclass(frozen=True, slots=True)
class TrackObservation:
    frame_index: int
    track_id: str
    category_id: int
    bbox: Box
    source: str
    mean_absolute_error: float | None = None


@dataclass(frozen=True, slots=True)
class UncertainRange:
    track_id: str
    first_frame_index: int
    last_frame_index: int
    reason: str


@dataclass(frozen=True, slots=True)
class TailCheck:
    track_id: str
    status: str


@dataclass(frozen=True, slots=True)
class TranslationResult:
    processed_frame_count: int
    observations: tuple[TrackObservation, ...]
    uncertain_ranges: tuple[UncertainRange, ...]
    tail_checks: tuple[TailCheck, ...]


@dataclass(slots=True)
class _Track:
    seed: TrackSeed
    left: int
    top: int
    template: Image.Image | None
    dx: int = 0
    dy: int = 0
    uncertain: tuple[int, str] | None = None

    def box(self) -> Box:
        x, y, width, height = self.seed.bbox
        return x + self.dx, y + self.dy, width, height


def track_indexed_video(
    decoder: VideoDecoder,
    source: Path,
    index: VideoIndex,
    frame_indices: Sequence[int],
    seeds: Sequence[TrackSeed],
    policy: TranslationPolicy = TranslationPolicy(),
    *,
    tail_anchors: Sequence[TrackSeed] = (),
) -> TranslationResult:
    """Decode precisely the frozen display-frame selection before tracking each frame."""
    previous = -1
    for offset in frame_indices:
        if type(offset) is not int or not previous < offset < len(index.frames):
            raise ValueError("Tracking selection must contain unique ordered display indices")
        previous = offset
    with closing(_decoded_frames(decoder, source, index, frame_indices)) as samples:
        return track_translation(samples, seeds, policy, tail_anchors=tail_anchors)


def _decoded_frames(
    decoder: VideoDecoder, source: Path, index: VideoIndex, frame_indices: Sequence[int],
) -> Iterator[TrackingFrame]:
    for frame_index in frame_indices:
        encoded = decoder.decode_png(source, index, frame_index)
        with Image.open(BytesIO(encoded)) as image:
            image.load()
            yield TrackingFrame(frame_index, image)


def track_translation(
    frames: Iterable[TrackingFrame],
    seeds: Sequence[TrackSeed],
    policy: TranslationPolicy = TranslationPolicy(),
    *,
    tail_anchors: Sequence[TrackSeed] = (),
) -> TranslationResult:
    """Use a fixed head template for tracking and compare tail anchors after tracking.

    Once a track loses its identity, later frames remain uncertain. Searching
    for a new identity after a gap would need a new human seed.
    """
    _validate_policy(policy)
    samples = iter(frames)
    initial = next(samples, None)
    if initial is None or not seeds:
        raise ValueError("Translation tracking requires frames and human seeds")
    size = initial.image.size
    _validate_frame(initial, size, -1)
    seed_ids = _validate_seeds(seeds, size)
    _validate_seeds(tail_anchors, size)
    for anchor in tail_anchors:
        if anchor.track_id not in seed_ids or seed_ids[anchor.track_id] != anchor.category_id:
            raise ValueError("A tail anchor must reference its original track and category")

    luminance = initial.image.convert("L")
    tracks = [_make_track(seed, luminance) for seed in seeds]
    observations = [
        TrackObservation(initial.frame_index, seed.track_id, seed.category_id, seed.bbox, "HUMAN_SEED")
        for seed in seeds
    ]
    previous = initial.frame_index
    count = 1
    for sample in samples:
        checkpoint()
        _validate_frame(sample, size, previous)
        previous = sample.frame_index
        count += 1
        luminance = sample.image.convert("L")
        matches = {}
        for track in tracks:
            if track.uncertain is not None:
                continue
            if track.template is None:
                track.uncertain = (sample.frame_index, "UNSUPPORTED_SEED")
                continue
            candidate, reason = _match_template(track, track.template, luminance, policy)
            if candidate is None:
                track.uncertain = (sample.frame_index, reason)
                continue
            dx, dy, error = candidate
            region = (track.left + dx, track.top + dy, *track.template.size)
            matches.setdefault(region, []).append((track, dx, dy, error))
        for claims in matches.values():
            if len(claims) != 1:
                for track, _, _, _ in claims:
                    track.uncertain = (sample.frame_index, "AMBIGUOUS_IDENTITY")
                continue
            track, dx, dy, error = claims[0]
            track.dx, track.dy = dx, dy
            observations.append(TrackObservation(
                sample.frame_index, track.seed.track_id, track.seed.category_id,
                track.box(), "TRACKER", error,
            ))

    ranges = []
    for track in tracks:
        if track.uncertain is not None:
            first, reason = track.uncertain
            ranges.append(UncertainRange(track.seed.track_id, first, previous, reason))
    by_id = {track.seed.track_id: track for track in tracks}
    checks = tuple(_check_tail(by_id[anchor.track_id], anchor) for anchor in tail_anchors)
    return TranslationResult(count, tuple(observations), tuple(ranges), checks)


def _validate_policy(policy: TranslationPolicy) -> None:
    if (type(policy.search_radius) is not int or not 1 <= policy.search_radius <= _MAX_RADIUS
            or not 0 <= policy.max_error <= 1 or not 0 < policy.min_margin <= 1):
        raise ValueError("Invalid translation tracking policy")


def _validate_frame(sample: TrackingFrame, size: tuple[int, int], previous: int) -> None:
    if (type(sample.frame_index) is not int or not previous < sample.frame_index <= _MAX_FRAME_INDEX
            or sample.image.size != size or min(size) <= 0):
        raise ValueError("Tracking frames must be ordered and have identical positive dimensions")


def _validate_seeds(seeds: Sequence[TrackSeed], size: tuple[int, int]) -> dict[str, int]:
    identities = {}
    for seed in seeds:
        try:
            identity = UUID(seed.track_id)
        except (ValueError, AttributeError, TypeError) as error:
            raise ValueError("Invalid track identifier") from error
        if (str(identity) != seed.track_id or identity.int == 0 or seed.track_id in identities
                or type(seed.category_id) is not int or seed.category_id < 0):
            raise ValueError("Track identities and categories must be valid and unique per anchor frame")
        if len(seed.bbox) != 4 or not all(
            type(value) is int or (type(value) is float and isfinite(value)) for value in seed.bbox
        ):
            raise ValueError("Tracking seed must have four finite coordinates")
        x, y, width, height = seed.bbox
        if not (0 <= x < size[0] and 0 <= y < size[1]
                and 0 < width <= size[0] - x and 0 < height <= size[1] - y):
            raise ValueError("Tracking seed lies outside its frame")
        identities[seed.track_id] = seed.category_id
    return identities


def _make_track(seed: TrackSeed, image: Image.Image) -> _Track:
    x, y, width, height = seed.bbox
    left, top, right, bottom = floor(x), floor(y), ceil(x + width), ceil(y + height)
    if (right - left) * (bottom - top) > _MAX_TEMPLATE_PIXELS:
        return _Track(seed, left, top, None)
    template = image.crop((left, top, right, bottom))
    minimum, maximum = template.getextrema()
    return _Track(seed, left, top, template if minimum != maximum else None)


def _match_template(
    track: _Track, template: Image.Image, image: Image.Image, policy: TranslationPolicy,
) -> tuple[tuple[int, int, float] | None, str]:
    patch_width, patch_height = template.size
    radius = policy.search_radius
    center_x, center_y = track.left + track.dx, track.top + track.dy
    x_range = range(max(0, center_x - radius), min(image.width - patch_width, center_x + radius) + 1)
    y_range = range(max(0, center_y - radius), min(image.height - patch_height, center_y + radius) + 1)
    best = second = inf
    best_offset = (0, 0)
    divisor = 255 * patch_width * patch_height
    for top in y_range:
        checkpoint()
        for left in x_range:
            patch = image.crop((left, top, left + patch_width, top + patch_height))
            histogram = ImageChops.difference(template, patch).histogram()
            error = sum(value * frequency for value, frequency in enumerate(histogram)) / divisor
            if error < best:
                best, second = error, best
                best_offset = left - track.left, top - track.top
            elif error < second:
                second = error
    if best > policy.max_error:
        return None, "TARGET_LOST"
    if second - best < policy.min_margin:
        return None, "AMBIGUOUS_MATCH"
    return (*best_offset, best), ""


def _check_tail(track: _Track, anchor: TrackSeed) -> TailCheck:
    if track.uncertain is not None:
        status = "NO_CANDIDATE"
    else:
        status = "MATCHES" if track.box() == anchor.bbox else "DISAGREES"
    return TailCheck(anchor.track_id, status)
