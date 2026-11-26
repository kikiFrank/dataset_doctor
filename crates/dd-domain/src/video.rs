//! Identity, presentation time and sampling for decoded frames in display order.

use std::collections::BTreeSet;
use std::error::Error;
use std::{fmt, ops::Range};

use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeBase {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rotation {
    None,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceStream {
    pub video_id: Uuid,
    pub source_digest: [u8; 32],
    pub stream_index: u32,
    pub time_base: Option<TimeBase>,
    pub rotation: Rotation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedFrame {
    pub pts: Option<i64>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameRef {
    pub source_video_id: Uuid,
    pub source_digest: [u8; 32],
    pub stream_index: u32,
    pub frame_index: u64,
    pub raw_pts: Option<i64>,
    pub pts_us: Option<i64>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeekPolicy {
    AtOrAfter,
    Nearest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameRange {
    Time { start_us: i64, end_us: i64 },
    Indices { start: u64, end: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplingMode {
    EveryNFrames { every_n: u64 },
    TimeInterval { interval_us: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplePurpose {
    Tracking,
    Dataset { include_keyframes: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameIndexError {
    NoFrames,
    InvalidSource,
    InvalidTimeBase,
    InvalidDimensions,
    SourceChanged,
    TimingUnavailable,
    FrameOutsideIndex,
    InvalidRange,
    InvalidInterval,
    KeyframeOutsideRange,
}

impl fmt::Display for FrameIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoFrames => "The decoder produced no display frames",
            Self::InvalidSource => "The decoded stream has no registered video identity",
            Self::InvalidTimeBase => "The decoded stream has an invalid time base",
            Self::InvalidDimensions => "A decoded frame has invalid dimensions",
            Self::SourceChanged => "The indexed video source has changed",
            Self::TimingUnavailable => "The decoded stream has no reliable display timeline",
            Self::FrameOutsideIndex => "The requested frame is outside the index",
            Self::InvalidRange => "The sampling range is invalid for this index and mode",
            Self::InvalidInterval => "The sampling interval must be positive and representable",
            Self::KeyframeOutsideRange => "A keyframe is outside the selected range",
        })
    }
}

impl Error for FrameIndexError {}

pub struct FrameIndex {
    source: SourceStream,
    frames: Vec<FrameRef>,
}

impl FrameIndex {
    pub fn from_display_order(
        source: SourceStream,
        frames: impl IntoIterator<Item = DecodedFrame>,
    ) -> Result<Self, FrameIndexError> {
        if source.video_id.is_nil() {
            return Err(FrameIndexError::InvalidSource);
        }
        if source
            .time_base
            .is_some_and(|clock| clock.numerator == 0 || clock.denominator == 0)
        {
            return Err(FrameIndexError::InvalidTimeBase);
        }
        let decoded: Vec<_> = frames.into_iter().collect();
        if decoded.is_empty() {
            return Err(FrameIndexError::NoFrames);
        }
        let normalized = source
            .time_base
            .and_then(|clock| normalized_times(&decoded, clock));
        let rotate = matches!(
            source.rotation,
            Rotation::Clockwise90 | Rotation::Clockwise270
        );
        let frames = decoded
            .into_iter()
            .enumerate()
            .map(|(index, frame)| {
                if frame.width == 0 || frame.height == 0 {
                    return Err(FrameIndexError::InvalidDimensions);
                }
                let (width, height) = if rotate {
                    (frame.height, frame.width)
                } else {
                    (frame.width, frame.height)
                };
                Ok(FrameRef {
                    source_video_id: source.video_id,
                    source_digest: source.source_digest,
                    stream_index: source.stream_index,
                    frame_index: index as u64,
                    raw_pts: frame.pts,
                    pts_us: normalized.as_ref().map(|timestamps| timestamps[index]),
                    width,
                    height,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { source, frames })
    }

    pub fn source(&self) -> SourceStream {
        self.source
    }

    pub fn resolve_index(
        &self,
        current_digest: [u8; 32],
        frame: u64,
    ) -> Result<FrameRef, FrameIndexError> {
        self.verify_source(current_digest)?;
        usize::try_from(frame)
            .ok()
            .and_then(|index| self.frames.get(index))
            .copied()
            .ok_or(FrameIndexError::FrameOutsideIndex)
    }

    pub fn resolve_time(
        &self,
        current_digest: [u8; 32],
        pts_us: i64,
        policy: SeekPolicy,
    ) -> Result<FrameRef, FrameIndexError> {
        self.verify_source(current_digest)?;
        self.require_timeline()?;
        let upper = first_at_or_after(&self.frames, pts_us);
        if pts_us < 0 || upper == self.frames.len() {
            return Err(FrameIndexError::FrameOutsideIndex);
        }
        let next = self.frames[upper];
        if policy == SeekPolicy::AtOrAfter || upper == 0 {
            return Ok(next);
        }
        let later = next.pts_us.ok_or(FrameIndexError::TimingUnavailable)?;
        let earlier = self.frames[upper - 1]
            .pts_us
            .ok_or(FrameIndexError::TimingUnavailable)?;
        if pts_us - earlier <= later - pts_us {
            Ok(self.frames[first_at_or_after(&self.frames, earlier)])
        } else {
            Ok(next)
        }
    }

    pub fn sample(
        &self,
        current_digest: [u8; 32],
        range: FrameRange,
        mode: SamplingMode,
        purpose: SamplePurpose,
        keyframes: &[u64],
    ) -> Result<Vec<FrameRef>, FrameIndexError> {
        self.verify_source(current_digest)?;
        let span = self.index_span(range)?;
        let mut selected = match mode {
            SamplingMode::EveryNFrames { every_n } => {
                let every_n = usize::try_from(every_n)
                    .ok()
                    .filter(|&interval| interval > 0)
                    .ok_or(FrameIndexError::InvalidInterval)?;
                span.clone().step_by(every_n).collect()
            }
            SamplingMode::TimeInterval { interval_us } => {
                let FrameRange::Time { start_us, end_us } = range else {
                    return Err(FrameIndexError::InvalidRange);
                };
                select_timed(
                    &self.frames[span.clone()],
                    span.start,
                    start_us,
                    end_us,
                    interval_us,
                )?
            }
        };
        let include = matches!(
            purpose,
            SamplePurpose::Tracking
                | SamplePurpose::Dataset {
                    include_keyframes: true
                }
        );
        for &frame in keyframes {
            let frame = usize::try_from(frame)
                .ok()
                .filter(|index| span.contains(index))
                .ok_or(FrameIndexError::KeyframeOutsideRange)?;
            if include {
                selected.insert(frame);
            }
        }
        Ok(selected
            .into_iter()
            .map(|index| self.frames[index])
            .collect())
    }

    fn verify_source(&self, current: [u8; 32]) -> Result<(), FrameIndexError> {
        if current != self.source.source_digest {
            return Err(FrameIndexError::SourceChanged);
        }
        Ok(())
    }

    fn require_timeline(&self) -> Result<(), FrameIndexError> {
        self.frames[0]
            .pts_us
            .map(|_| ())
            .ok_or(FrameIndexError::TimingUnavailable)
    }

    fn index_span(&self, range: FrameRange) -> Result<Range<usize>, FrameIndexError> {
        match range {
            FrameRange::Time { start_us, end_us } if start_us >= 0 && end_us > start_us => {
                self.require_timeline()?;
                Ok(first_at_or_after(&self.frames, start_us)
                    ..first_at_or_after(&self.frames, end_us))
            }
            FrameRange::Indices { start, end }
                if end > start && end <= self.frames.len() as u64 =>
            {
                Ok(start as usize..end as usize)
            }
            _ => Err(FrameIndexError::InvalidRange),
        }
    }
}

fn normalized_times(frames: &[DecodedFrame], clock: TimeBase) -> Option<Vec<i64>> {
    let first = frames.first()?.pts?;
    let mut previous = first;
    frames
        .iter()
        .map(|frame| {
            let pts = frame.pts?;
            if pts < previous {
                return None;
            }
            previous = pts;
            let ticks = i128::from(pts) - i128::from(first);
            let micros =
                ticks * i128::from(clock.numerator) * 1_000_000 / i128::from(clock.denominator);
            i64::try_from(micros).ok()
        })
        .collect()
}

fn first_at_or_after(frames: &[FrameRef], timestamp: i64) -> usize {
    frames.partition_point(|frame| frame.pts_us.is_some_and(|time| time < timestamp))
}

fn select_timed(
    frames: &[FrameRef],
    offset: usize,
    start_us: i64,
    end_us: i64,
    interval_us: u64,
) -> Result<BTreeSet<usize>, FrameIndexError> {
    if interval_us == 0 {
        return Err(FrameIndexError::InvalidInterval);
    }
    let interval = i128::from(interval_us);
    let origin = i128::from(start_us);
    let end = i128::from(end_us);
    let mut target = origin;
    let mut selected = BTreeSet::new();
    while target < end {
        let index = first_at_or_after(frames, target as i64);
        let Some(frame) = frames.get(index) else {
            break;
        };
        let time = i128::from(frame.pts_us.ok_or(FrameIndexError::TimingUnavailable)?);
        selected.insert(offset + index);
        // Advance past every target for which this same frame would be picked.
        target = origin + ((time - origin) / interval + 1) * interval;
    }
    Ok(selected)
}
