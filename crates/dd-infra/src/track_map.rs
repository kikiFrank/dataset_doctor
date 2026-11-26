//! Current Asset-owned track mappings kept alongside standard COCO annotations.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::json::{JsonContracts, JsonProfile};
use crate::video_annotation_store::{VideoFrameReference, read_asset_artifact};

const KIND: &str = "VIDEO_TRACK_MAP";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTrackBinding {
    pub local_annotation_id: u64,
    pub track_id: Uuid,
    pub tracking_task_id: Uuid,
    pub tracking_attempt_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackMap {
    pub contract_version: u8,
    pub asset_id: Uuid,
    pub annotation_digest: String,
    pub segment_id: Uuid,
    pub frame: VideoFrameReference,
    pub bindings: Vec<ReviewTrackBinding>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackMapEntry {
    pub reference: ArtifactReference,
    pub relative_path: String,
}

impl TrackMapEntry {
    #[cfg(feature = "execution-rpc")]
    pub fn from_metadata(metadata: &str) -> io::Result<Option<Self>> {
        let document: serde_json::Value =
            serde_json::from_str(metadata).map_err(io::Error::other)?;
        match (
            document.get("current_track_map_ref"),
            document.get("current_track_map_entry"),
        ) {
            (None, None) => Ok(None),
            (Some(reference), Some(entry)) => {
                let reference: ArtifactReference =
                    serde_json::from_value(reference.clone()).map_err(io::Error::other)?;
                let entry: Self =
                    serde_json::from_value(entry.clone()).map_err(io::Error::other)?;
                if entry.reference != reference {
                    return Err(invalid());
                }
                Ok(Some(entry))
            }
            _ => Err(invalid()),
        }
    }

    #[cfg(feature = "execution-rpc")]
    pub fn publish(
        root: &Path,
        contracts: &JsonContracts,
        map: &TrackMap,
        artifact: Uuid,
    ) -> io::Result<Self> {
        use sha2::{Digest, Sha256};

        let bytes = serde_json::to_vec(map).map_err(io::Error::other)?;
        parse_map(contracts, map.asset_id, &map.annotation_digest, &bytes)?;
        let entry = Self {
            reference: ArtifactReference {
                artifact_id: artifact,
                owner: ArtifactOwner::Asset(map.asset_id),
                kind: KIND.into(),
                media_type: "application/json".into(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size_bytes: bytes.len().to_string(),
                schema_id: None,
            },
            relative_path: track_map_path(map.asset_id, artifact),
        };
        crate::paths::publish_immutable(root, Path::new(&entry.relative_path), &bytes)?;
        Ok(entry)
    }

    pub fn read(
        &self,
        root: &Path,
        contracts: &JsonContracts,
        asset: Uuid,
        annotation_digest: &str,
    ) -> io::Result<TrackMap> {
        if self.relative_path != track_map_path(asset, self.reference.artifact_id) {
            return Err(invalid());
        }
        let bytes = read_asset_artifact(root, asset, KIND, &self.relative_path, &self.reference)?;
        parse_map(contracts, asset, annotation_digest, &bytes)
    }
}

fn track_map_path(asset: Uuid, artifact: Uuid) -> String {
    format!("artifacts/images/{asset}/track-maps/{artifact}.json")
}

fn parse_map(
    contracts: &JsonContracts,
    asset: Uuid,
    annotation_digest: &str,
    bytes: &[u8],
) -> io::Result<TrackMap> {
    let document = contracts
        .validate(JsonProfile::TrackMap, bytes)
        .map_err(io::Error::other)?;
    let map: TrackMap = serde_json::from_value(document).map_err(io::Error::other)?;
    let frame = &map.frame;
    let mut annotation_ids = BTreeSet::new();
    let mut track_ids = BTreeSet::new();
    if map.asset_id != asset
        || map.annotation_digest != annotation_digest
        || map.segment_id.is_nil()
        || frame.source_video_id.is_nil()
        || frame.frame_index.parse::<u64>().is_err()
        || frame
            .pts_us
            .as_deref()
            .map(str::parse::<i64>)
            .transpose()
            .is_err()
        || frame
            .timestamp_ms
            .as_deref()
            .map(str::parse::<u64>)
            .transpose()
            .is_err()
        || !matches!(frame.image_ref.owner, ArtifactOwner::Task(task) if !task.is_nil())
        || map.bindings.iter().any(|binding| {
            binding.track_id.is_nil()
                || binding.tracking_task_id.is_nil()
                || binding.tracking_attempt_id.is_nil()
                || !annotation_ids.insert(binding.local_annotation_id)
                || !track_ids.insert(binding.track_id)
        })
    {
        return Err(invalid());
    }
    Ok(map)
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid current track map")
}
