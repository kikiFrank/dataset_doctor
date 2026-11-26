use std::path::Path;

use dd_domain::video::FrameRange;
use dd_protocol::v1::{TrackingFrameInput, TrackingSeedInput, VideoTrackingInput, WorkerInputFile};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{CAPABILITY, TRANSLATION_PROFILE_ID, TrackingError};
use crate::input_fingerprint::{
    FrozenInputManifest, deserialize_manifest_u32 as manifest_u32, freeze_input_manifest,
};
use crate::json::JsonContracts;
use crate::task_store::format_digest;
use crate::video_keyframes::{ResolvedVideoSampling, VideoAnnotationService};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TrackingBinding {
    pub segment_id: Uuid,
    pub tracker_profile_id: Uuid,
    #[serde(deserialize_with = "manifest_u32")]
    pub stream_index: u32,
    pub frames: Vec<FrameBinding>,
    pub seeds: Vec<SeedBinding>,
    pub tail_anchors: Vec<SeedBinding>,
    pub policy: TrackingPolicy,
    pub limits: TrackingLimits,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrameBinding {
    pub frame_index: String,
    pub raw_pts: Option<String>,
    #[serde(deserialize_with = "manifest_u32")]
    pub width: u32,
    #[serde(deserialize_with = "manifest_u32")]
    pub height: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SeedBinding {
    pub track_id: Uuid,
    pub category_id: String,
    pub bbox_xywh: [f64; 4],
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TrackingPolicy {
    #[serde(deserialize_with = "manifest_u32")]
    pub search_radius: u32,
    pub max_error: f64,
    pub min_margin: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TrackingLimits {
    pub max_pixels: String,
    #[serde(deserialize_with = "manifest_u32")]
    pub max_frames: u32,
    #[serde(deserialize_with = "manifest_u32")]
    pub timeout_ms: u32,
}

pub(super) fn freeze_plan(
    contracts: &JsonContracts,
    work: &VideoAnnotationService,
    session: Uuid,
    plan: &ResolvedVideoSampling,
    selection: Value,
) -> Result<FrozenInputManifest, TrackingError> {
    let request = plan.request;
    let mut keyframes = plan
        .keyframes
        .iter()
        .map(|keyframe| Ok((decimal::<u64>(&keyframe.frame_ref.frame_index)?, keyframe)))
        .collect::<Result<Vec<_>, TrackingError>>()?;
    keyframes.sort_unstable_by_key(|(index, _)| *index);
    let (head_index, seeds) = keyframes
        .into_iter()
        .find_map(|(index, keyframe)| {
            let result = (|| {
                let draft = work
                    .load_draft(
                        session,
                        request.source_video_id,
                        keyframe.annotation_ref.clone(),
                    )
                    .map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
                if draft.frame_ref != keyframe.frame_ref
                    || draft.target_dataset_id != request.target_dataset_id
                    || draft.label_schema_digest != plan.label_schema_digest
                {
                    return Err(TrackingError::InvalidBinding);
                }
                let boxes = draft.coco_document["annotations"]
                    .as_array()
                    .ok_or(TrackingError::InvalidBinding)?;
                boxes
                    .iter()
                    .map(|annotation| {
                        seed_binding(
                            request.source_video_id,
                            request.segment_id,
                            index,
                            annotation,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })();
            match result {
                Ok(seeds) if seeds.is_empty() => None,
                other => Some(other.map(|seeds| (index, seeds))),
            }
        })
        .ok_or(TrackingError::InvalidBinding)??;
    let first = plan
        .frames
        .iter()
        .position(|frame| frame.frame_index == head_index)
        .ok_or(TrackingError::InvalidBinding)?;
    let frames: Vec<_> = plan.frames[first..]
        .iter()
        .map(|frame| FrameBinding {
            frame_index: frame.frame_index.to_string(),
            raw_pts: frame.raw_pts.map(|pts| pts.to_string()),
            width: frame.width,
            height: frame.height,
        })
        .collect();
    if frames.len() > 512 || seeds.len() > 128 {
        return Err(TrackingError::InvalidBinding);
    }
    let binding = TrackingBinding {
        segment_id: request.segment_id,
        tracker_profile_id: TRANSLATION_PROFILE_ID,
        stream_index: plan.source.stream_index,
        frames,
        seeds,
        // Independent COCO files do not specify which tail box belongs to a head track.
        tail_anchors: vec![],
        policy: TrackingPolicy {
            search_radius: 8,
            max_error: 0.08,
            min_margin: 0.01,
        },
        limits: TrackingLimits {
            max_pixels: "100000000".into(),
            max_frames: 100_000,
            timeout_ms: 30_000,
        },
    };
    let FrameRange::Time { start_us, end_us } = plan.range else {
        return Err(TrackingError::InvalidBinding);
    };
    freeze_input_manifest(contracts, json!({
        "capability_id": CAPABILITY, "implementation_version": "1", "model_identity": null,
        "dataset_id":request.target_dataset_id,"label_schema_digest":plan.label_schema_digest,
        "assets":[{"asset_id":request.source_video_id,
            "media_digest":format_digest(&plan.source.source_digest),
            "annotation_digest":null,"source_metadata_digest":plan.video_annotation_manifest_ref.sha256}],
        "resolved_config":{
            "tracking_binding":binding,"selection":selection,"sampling_config_id":plan.sampling_config_id,
            "segment_revision":plan.segment_revision.to_string(),
            "start_us":start_us.to_string(),"end_us":end_us.to_string(),
            "unseeded_selected_frames":first,
            "video_annotation_manifest_ref":plan.video_annotation_manifest_ref,
        },
    })).map_err(TrackingError::Fingerprint)
}

fn seed_binding(
    video: Uuid,
    segment: Uuid,
    frame: u64,
    annotation: &Value,
) -> Result<SeedBinding, TrackingError> {
    let local_id = annotation["id"]
        .as_u64()
        .ok_or(TrackingError::InvalidBinding)?;
    let category = annotation["category_id"]
        .as_u64()
        .ok_or(TrackingError::InvalidBinding)?;
    let bbox_xywh = serde_json::from_value(annotation["bbox"].clone())
        .map_err(|_| TrackingError::InvalidBinding)?;
    let mut identity = Sha256::new();
    identity.update(b"datasetdoctor.video.track.v1\0");
    identity.update(video.as_bytes());
    identity.update(segment.as_bytes());
    identity.update(frame.to_le_bytes());
    identity.update(local_id.to_le_bytes());
    let digest = identity.finalize();
    Ok(SeedBinding {
        track_id: uuid::Builder::from_custom_bytes(
            digest[..16].try_into().expect("16 digest bytes"),
        )
        .into_uuid(),
        category_id: category.to_string(),
        bbox_xywh,
    })
}

impl TrackingBinding {
    pub(super) fn parameters(
        &self,
        source: &Path,
        digest: &[u8; 32],
        manifest: &Path,
        manifest_digest: &[u8; 32],
    ) -> Result<VideoTrackingInput, TrackingError> {
        let locator = |path: &Path, sha: &[u8; 32]| -> Result<WorkerInputFile, TrackingError> {
            Ok(WorkerInputFile {
                path: path.to_str().ok_or(TrackingError::InvalidBinding)?.into(),
                sha256: format_digest(sha),
            })
        };
        Ok(VideoTrackingInput {
            source_video: Some(locator(source, digest)?),
            input_manifest: Some(locator(manifest, manifest_digest)?),
            stream_index: self.stream_index,
            segment_id: self.segment_id.to_string(),
            tracker_profile_id: self.tracker_profile_id.to_string(),
            frames: self
                .frames
                .iter()
                .map(|frame| {
                    Ok(TrackingFrameInput {
                        frame_index: Some(decimal(&frame.frame_index)?),
                        expected_raw_pts: frame.raw_pts.as_deref().map(decimal).transpose()?,
                        display_width: frame.width,
                        display_height: frame.height,
                    })
                })
                .collect::<Result<_, TrackingError>>()?,
            seeds: self
                .seeds
                .iter()
                .map(SeedBinding::parameters)
                .collect::<Result<_, _>>()?,
            tail_anchors: self
                .tail_anchors
                .iter()
                .map(SeedBinding::parameters)
                .collect::<Result<_, _>>()?,
            search_radius: self.policy.search_radius,
            max_error: self.policy.max_error,
            min_margin: self.policy.min_margin,
            max_pixels: decimal(&self.limits.max_pixels)?,
            max_frames: self.limits.max_frames,
            timeout_ms: self.limits.timeout_ms,
            output_directory: String::new(),
        })
    }
}

impl SeedBinding {
    fn parameters(&self) -> Result<TrackingSeedInput, TrackingError> {
        Ok(TrackingSeedInput {
            track_id: self.track_id.to_string(),
            category_id: decimal(&self.category_id)?,
            bbox_xywh: self.bbox_xywh.to_vec(),
        })
    }
}

fn decimal<T: std::str::FromStr + ToString>(text: &str) -> Result<T, TrackingError> {
    let parsed: T = text.parse().map_err(|_| TrackingError::InvalidBinding)?;
    if parsed.to_string() != text {
        return Err(TrackingError::InvalidBinding);
    }
    Ok(parsed)
}
