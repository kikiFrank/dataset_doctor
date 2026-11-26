//! Resolve sampling inputs from the authoritative index and a saved video segment.

use std::collections::BTreeSet;

use dd_domain::video::{FrameRange, FrameRef, SamplePurpose, SamplingMode, SourceStream};
use uuid::Uuid;

use super::{VideoAnnotationError, VideoAnnotationService};
use crate::task_store::parse_digest;
use crate::video_annotation_store::{VideoArtifactReference, VideoKeyframe, read_keyframe};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSamplingMode {
    EveryNFrames { every_n: u32 },
    TimeInterval { interval_ms: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSamplingPurpose {
    Tracking {
        supports_time_interval: bool,
    },
    Dataset {
        include_keyframes: bool,
        max_frames: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct VideoSamplingRequest {
    pub source_video_id: Uuid,
    pub segment_id: Uuid,
    pub target_dataset_id: Uuid,
    pub mode: VideoSamplingMode,
    pub purpose: VideoSamplingPurpose,
}

#[derive(Debug)]
pub struct ResolvedVideoSampling {
    pub sampling_config_id: Uuid,
    pub request: VideoSamplingRequest,
    pub segment_revision: u64,
    pub range: FrameRange,
    pub source: SourceStream,
    pub label_schema_digest: String,
    pub video_annotation_manifest_ref: VideoArtifactReference,
    pub frames: Vec<FrameRef>,
    pub keyframes: Vec<VideoKeyframe>,
}

impl VideoAnnotationService {
    pub fn resolve_sampling(
        &self,
        session: Uuid,
        request: VideoSamplingRequest,
    ) -> Result<ResolvedVideoSampling, VideoAnnotationError> {
        let _guard = self.owner.lock_final_operations()?;
        self.resolve_sampling_locked(session, request)
    }

    pub(super) fn resolve_sampling_locked(
        &self,
        session: Uuid,
        request: VideoSamplingRequest,
    ) -> Result<ResolvedVideoSampling, VideoAnnotationError> {
        let mode = request.sampling_mode()?;
        let purpose = match request.purpose {
            VideoSamplingPurpose::Tracking { .. } => SamplePurpose::Tracking,
            VideoSamplingPurpose::Dataset {
                include_keyframes, ..
            } => SamplePurpose::Dataset { include_keyframes },
        };
        let snapshot = self.snapshot(session, request.source_video_id)?;
        let (Some(manifest), Some(reference)) =
            self.load_manifest(&snapshot, request.source_video_id)?
        else {
            return Err(VideoAnnotationError::InvalidInput);
        };
        let segment = manifest
            .segments
            .into_iter()
            .find(|segment| {
                segment.segment_id == request.segment_id
                    && segment.target_dataset_id == request.target_dataset_id
            })
            .ok_or(VideoAnnotationError::InvalidInput)?;
        let current_dataset = self
            .owner
            .dataset_detail(session, request.target_dataset_id)?;
        let label_schema_digest = current_dataset.summary.schema_fingerprint;
        if !manifest
            .target_contexts
            .get(&request.target_dataset_id)
            .is_some_and(|context| context.label_schema_digest == label_schema_digest)
        {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        let range = FrameRange::Time {
            start_us: segment
                .start_us
                .parse()
                .map_err(|_| VideoAnnotationError::InvalidInput)?,
            end_us: segment
                .end_us
                .parse()
                .map_err(|_| VideoAnnotationError::InvalidInput)?,
        };
        let segment_revision = segment
            .revision
            .parse()
            .map_err(|_| VideoAnnotationError::InvalidInput)?;
        let source = parse_digest(&snapshot.digest).ok_or(VideoAnnotationError::InvalidInput)?;
        let index = self.online_video_index(session, request.source_video_id, &snapshot.digest)?;
        let keyframe_indices = segment
            .keyframes
            .iter()
            .map(|keyframe| {
                keyframe
                    .frame_ref
                    .frame_index
                    .parse::<u64>()
                    .map_err(|_| VideoAnnotationError::InvalidInput)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let frames = index
            .sample(source, range, mode, purpose, &keyframe_indices)
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
        if frames.is_empty() {
            return Err(VideoAnnotationError::InvalidInput);
        }
        if let VideoSamplingPurpose::Dataset {
            max_frames: Some(limit),
            ..
        } = request.purpose
            && frames.len() > limit as usize
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let selected: BTreeSet<_> = frames.iter().map(|frame| frame.frame_index).collect();
        let mut keyframes = Vec::new();
        let mut has_seed = false;
        for (keyframe, index) in segment.keyframes.into_iter().zip(keyframe_indices) {
            if selected.contains(&index) {
                self.verify_frame(session, &keyframe.frame_ref)?;
                if matches!(purpose, SamplePurpose::Tracking) && !has_seed {
                    let draft = read_keyframe(
                        &snapshot.root,
                        request.source_video_id,
                        &keyframe.annotation_ref,
                    )?;
                    has_seed = draft.coco_document["annotations"]
                        .as_array()
                        .is_some_and(|boxes| !boxes.is_empty());
                }
                keyframes.push(keyframe);
            }
        }
        if matches!(purpose, SamplePurpose::Tracking) && !has_seed {
            return Err(VideoAnnotationError::InvalidInput);
        }
        Ok(ResolvedVideoSampling {
            sampling_config_id: Uuid::new_v4(),
            request,
            segment_revision,
            range,
            source: index.source(),
            label_schema_digest,
            video_annotation_manifest_ref: reference,
            frames,
            keyframes,
        })
    }
}

impl VideoSamplingRequest {
    fn sampling_mode(self) -> Result<SamplingMode, VideoAnnotationError> {
        let supports_time = !matches!(
            self.purpose,
            VideoSamplingPurpose::Tracking {
                supports_time_interval: false
            }
        );
        let frame_limit = if matches!(self.purpose, VideoSamplingPurpose::Tracking { .. }) {
            1_000
        } else {
            100_000
        };
        match self.mode {
            VideoSamplingMode::EveryNFrames { every_n } if (1..=frame_limit).contains(&every_n) => {
                Ok(SamplingMode::EveryNFrames {
                    every_n: u64::from(every_n),
                })
            }
            VideoSamplingMode::TimeInterval { interval_ms }
                if supports_time && (1..=3_600_000).contains(&interval_ms) =>
            {
                Ok(SamplingMode::TimeInterval {
                    interval_us: u64::from(interval_ms) * 1_000,
                })
            }
            _ => Err(VideoAnnotationError::InvalidInput),
        }
    }
}
