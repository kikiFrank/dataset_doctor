use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{LoadedTrackingTask, TrackingError};
use crate::artifact_manifest::{
    MANIFEST_FILE, TaskManifestScope, parse_stored_manifest_reference, verify_task_result_manifest,
};
use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::json::{JsonContracts, strict_json};
use crate::paths::open_scoped_file;
use crate::task_store::format_digest;

const BUNDLE_FILE: &str = "video-tracks.json";
const BUNDLE_KIND: &str = "VIDEO_TRACK_BUNDLE";
const BUNDLE_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct VideoTrackBundle {
    pub reference: ArtifactReference,
    pub target_dataset_id: Uuid,
    pub segment_revision: u64,
    pub label_schema_digest: String,
    pub data: VideoTrackData,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTrackData {
    pub contract_version: u8,
    pub source_video_id: Uuid,
    pub source_digest: String,
    pub stream_index: u32,
    pub segment_id: Uuid,
    pub tracker_profile_id: Uuid,
    pub algorithm: String,
    pub input_fingerprint: String,
    pub processed_frame_count: usize,
    pub observations: Vec<TrackObservation>,
    pub uncertain_ranges: Vec<TrackUncertainRange>,
    pub tail_checks: Vec<TrackTailCheck>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackObservation {
    pub frame_index: String,
    pub track_id: Uuid,
    pub category_id: String,
    pub bbox_xywh: [f64; 4],
    pub source: String,
    pub mean_absolute_error: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackUncertainRange {
    pub track_id: Uuid,
    pub first_frame_index: String,
    pub last_frame_index: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackTailCheck {
    pub track_id: Uuid,
    pub status: String,
}

pub(super) fn read_bundle(
    task: Uuid,
    loaded: &LoadedTrackingTask,
    contracts: &JsonContracts,
) -> Result<VideoTrackBundle, TrackingError> {
    let invalid = TrackingError::ResultUnconfirmed;
    let record = &loaded.record;
    let attempt = record.attempt.ok_or(TrackingError::ResultUnconfirmed)?;
    let relative = PathBuf::from(format!("artifacts/tasks/{task}/{attempt}"));
    if record.output.as_deref() != Some(relative.join(MANIFEST_FILE).as_path()) {
        return Err(invalid);
    }
    let directory = record.root.join(relative);
    if directory.canonicalize()? != directory {
        return Err(invalid);
    }
    let reference = parse_stored_manifest_reference(
        record
            .result_reference
            .as_deref()
            .ok_or(TrackingError::ResultUnconfirmed)?,
        task,
    )
    .map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
    let seal = verify_task_result_manifest(
        &directory,
        contracts,
        &TaskManifestScope {
            task,
            attempt,
            input_fingerprint: loaded.frozen.input_fingerprint,
        },
        &reference,
    )
    .map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
    let [entry] = seal.entries() else {
        return Err(invalid);
    };
    if entry.relative_path != BUNDLE_FILE
        || entry.kind != BUNDLE_KIND
        || entry.media_type != "application/json"
        || entry.size_bytes > BUNDLE_LIMIT
    {
        return Err(invalid);
    }
    let mut bytes = Vec::new();
    open_scoped_file(&directory, Path::new(BUNDLE_FILE))?
        .take(entry.size_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != entry.size_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
    {
        return Err(invalid);
    }
    let value =
        strict_json(&bytes).map_err(|error| TrackingError::TaskEvidence(Box::new(error)))?;
    let data: VideoTrackData =
        serde_json::from_value(value).map_err(|_| TrackingError::ResultUnconfirmed)?;
    validate_bundle(loaded, &data)?;
    let reference = ArtifactReference {
        artifact_id: Uuid::parse_str(&entry.artifact_id)
            .map_err(|_| TrackingError::ResultUnconfirmed)?,
        owner: ArtifactOwner::Task(task),
        kind: entry.kind.clone(),
        media_type: entry.media_type.clone(),
        sha256: entry.sha256.clone(),
        size_bytes: entry.size_bytes.to_string(),
        schema_id: entry.schema_id.clone(),
    };
    Ok(VideoTrackBundle {
        reference,
        target_dataset_id: loaded.record.dataset,
        segment_revision: loaded.segment_revision,
        label_schema_digest: loaded.schema.clone(),
        data,
    })
}

fn validate_bundle(
    loaded: &LoadedTrackingTask,
    data: &VideoTrackData,
) -> Result<(), TrackingError> {
    let binding = &loaded.binding;
    let invalid = || TrackingError::ResultUnconfirmed;
    if data.contract_version != 1
        || data.source_video_id != loaded.video
        || data.source_digest != format_digest(&loaded.digest)
        || data.stream_index != binding.stream_index
        || data.segment_id != binding.segment_id
        || data.tracker_profile_id != binding.tracker_profile_id
        || data.algorithm != "translation_template.v1"
        || data.input_fingerprint != format_digest(&loaded.frozen.input_fingerprint)
        || data.processed_frame_count != binding.frames.len()
        || !data.tail_checks.is_empty()
    {
        return Err(invalid());
    }
    let frames: BTreeMap<_, _> = binding
        .frames
        .iter()
        .enumerate()
        .map(|(position, frame)| (frame.frame_index.as_str(), (position, frame)))
        .collect();
    let seeds: BTreeMap<_, _> = binding
        .seeds
        .iter()
        .map(|seed| (seed.track_id, seed))
        .collect();
    let mut lost = BTreeMap::new();
    let last = &binding.frames.last().ok_or_else(invalid)?.frame_index;
    for range in &data.uncertain_ranges {
        let (position, _) = frames
            .get(range.first_frame_index.as_str())
            .ok_or_else(invalid)?;
        if *position == 0
            || !seeds.contains_key(&range.track_id)
            || &range.last_frame_index != last
            || !matches!(
                range.reason.as_str(),
                "UNSUPPORTED_SEED" | "TARGET_LOST" | "AMBIGUOUS_MATCH" | "AMBIGUOUS_IDENTITY"
            )
            || lost.insert(range.track_id, *position).is_some()
        {
            return Err(invalid());
        }
    }
    let mut present = BTreeSet::new();
    for observation in &data.observations {
        let seed = seeds.get(&observation.track_id).ok_or_else(invalid)?;
        let &(position, frame) = frames
            .get(observation.frame_index.as_str())
            .ok_or_else(invalid)?;
        let last_observed = lost
            .get(&observation.track_id)
            .copied()
            .unwrap_or(binding.frames.len());
        if position >= last_observed
            || !present.insert((observation.track_id, position))
            || observation.category_id != seed.category_id
            || !valid_box(observation.bbox_xywh, frame.width, frame.height)
        {
            return Err(invalid());
        }
        if position == 0 {
            if observation.source != "HUMAN_SEED"
                || observation.bbox_xywh != seed.bbox_xywh
                || observation.mean_absolute_error.is_some()
            {
                return Err(invalid());
            }
        } else if observation.source != "TRACKER"
            || !observation
                .mean_absolute_error
                .is_some_and(|score| (0.0..=binding.policy.max_error).contains(&score))
            || observation.bbox_xywh[2..] != seed.bbox_xywh[2..]
        {
            return Err(invalid());
        }
    }
    let expected_count: usize = seeds
        .keys()
        .map(|track| lost.get(track).copied().unwrap_or(binding.frames.len()))
        .sum();
    if present.len() != expected_count {
        return Err(invalid());
    }
    Ok(())
}

fn valid_box([x, y, width, height]: [f64; 4], image_width: u32, image_height: u32) -> bool {
    [x, y, width, height].into_iter().all(f64::is_finite)
        && x >= 0.0
        && y >= 0.0
        && width > 0.0
        && height > 0.0
        && width <= f64::from(image_width) - x
        && height <= f64::from(image_height) - y
}
