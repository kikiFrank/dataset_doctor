use dd_domain::video::{
    DecodedFrame, FrameIndex, FrameIndexError, FrameRange, Rotation, SamplePurpose, SamplingMode,
    SeekPolicy, SourceStream, TimeBase,
};
use uuid::Uuid;

const SOURCE: [u8; 32] = [5; 32];

fn display_index(pts: &[Option<i64>], rotation: Rotation) -> FrameIndex {
    FrameIndex::from_display_order(
        SourceStream {
            video_id: Uuid::new_v4(),
            source_digest: SOURCE,
            stream_index: 2,
            time_base: Some(TimeBase {
                numerator: 1,
                denominator: 1000,
            }),
            rotation,
        },
        pts.iter().map(|&pts| DecodedFrame {
            pts,
            width: 9,
            height: 6,
        }),
    )
    .unwrap()
}

fn ids(frames: impl IntoIterator<Item = dd_domain::video::FrameRef>) -> Vec<u64> {
    frames.into_iter().map(|frame| frame.frame_index).collect()
}

#[test]
fn display_identity_preserves_raw_pts_normalizes_the_origin_and_rotates_dimensions() {
    let index = display_index(
        &[Some(-40), Some(-13), Some(-13), Some(17)],
        Rotation::Clockwise90,
    );
    let first = index.resolve_index(SOURCE, 0).unwrap();
    assert_eq!(first.pts_us, Some(0));
    assert_eq!(first.raw_pts, Some(-40));
    assert_eq!((first.width, first.height), (6, 9));
    assert_eq!(first.stream_index, 2);
    assert_eq!(first.source_digest, SOURCE);
    let left = index
        .resolve_time(SOURCE, 27_000, SeekPolicy::AtOrAfter)
        .unwrap();
    let right = index.resolve_index(SOURCE, 2).unwrap();
    assert_eq!((left.frame_index, right.frame_index), (1, 2));
    assert_eq!(left.pts_us, right.pts_us);
    assert_eq!(
        index
            .resolve_time(SOURCE, 55_000, SeekPolicy::Nearest)
            .unwrap()
            .frame_index,
        3
    );
    assert_eq!(
        index.resolve_index([6; 32], 0),
        Err(FrameIndexError::SourceChanged)
    );
}

#[test]
fn sampling_deduplicates_time_targets_and_preserves_half_open_keyframe_rules() {
    let index = display_index(
        &[
            Some(200),
            Some(220),
            Some(220),
            Some(260),
            Some(340),
            Some(395),
        ],
        Rotation::None,
    );
    let within = FrameRange::Time {
        start_us: 0,
        end_us: 141_000,
    };
    let time = SamplingMode::TimeInterval {
        interval_us: 25_000,
    };
    assert_eq!(
        ids(index
            .sample(
                SOURCE,
                within,
                time,
                SamplePurpose::Dataset {
                    include_keyframes: false
                },
                &[2]
            )
            .unwrap()),
        [0, 3, 4]
    );
    assert_eq!(
        ids(index
            .sample(
                SOURCE,
                within,
                time,
                SamplePurpose::Dataset {
                    include_keyframes: true
                },
                &[2, 2, 4]
            )
            .unwrap()),
        [0, 2, 3, 4]
    );
    let new_range = FrameRange::Time {
        start_us: 20_000,
        end_us: 140_000,
    };
    let frames = SamplingMode::EveryNFrames { every_n: 2 };
    assert_eq!(
        ids(index
            .sample(SOURCE, new_range, frames, SamplePurpose::Tracking, &[2])
            .unwrap()),
        [1, 2, 3]
    );
    assert_eq!(
        index.sample(SOURCE, new_range, frames, SamplePurpose::Tracking, &[4]),
        Err(FrameIndexError::KeyframeOutsideRange)
    );
}

#[test]
fn missing_or_unordered_pts_leave_index_sampling_available_without_guessing_times() {
    for pts in [[Some(7), None, Some(9)], [Some(7), Some(9), Some(8)]] {
        let index = display_index(&pts, Rotation::None);
        assert_eq!(
            index.resolve_time(SOURCE, 1, SeekPolicy::Nearest),
            Err(FrameIndexError::TimingUnavailable)
        );
        assert_eq!(index.resolve_index(SOURCE, 1).unwrap().pts_us, None);
        assert_eq!(
            ids(index
                .sample(
                    SOURCE,
                    FrameRange::Indices { start: 1, end: 3 },
                    SamplingMode::EveryNFrames { every_n: 1 },
                    SamplePurpose::Tracking,
                    &[]
                )
                .unwrap()),
            [1, 2]
        );
    }
}

#[test]
fn rational_time_uses_the_raw_time_base_without_accumulating_frame_rounding() {
    let index = FrameIndex::from_display_order(
        SourceStream {
            video_id: Uuid::new_v4(),
            source_digest: SOURCE,
            stream_index: 0,
            time_base: Some(TimeBase {
                numerator: 1001,
                denominator: 30_000,
            }),
            rotation: Rotation::None,
        },
        [-2, -1, 0, 1].map(|pts| DecodedFrame {
            pts: Some(pts),
            width: 16,
            height: 16,
        }),
    )
    .unwrap();
    let resolved = (0..4)
        .map(|position| index.resolve_index(SOURCE, position).unwrap().pts_us)
        .collect::<Vec<_>>();
    assert_eq!(
        resolved,
        [Some(0), Some(33_366), Some(66_733), Some(100_100)]
    );
    assert_eq!(
        index.sample(
            SOURCE,
            FrameRange::Time {
                start_us: 0,
                end_us: 120_000
            },
            SamplingMode::TimeInterval { interval_us: 0 },
            SamplePurpose::Tracking,
            &[]
        ),
        Err(FrameIndexError::InvalidInterval)
    );
}
