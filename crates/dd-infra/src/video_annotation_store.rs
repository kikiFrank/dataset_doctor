//! Immutable video work artifacts shared by editing, backup and restore.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::paths::open_scoped_file;

pub(crate) const VIDEO_WORK_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum VideoArtifactOwner {
    Task(Uuid),
    Asset(Uuid),
    Review(Uuid),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoArtifactReference {
    pub artifact_id: Uuid,
    pub owner: VideoArtifactOwner,
    pub kind: String,
    pub media_type: String,
    pub sha256: String,
    pub size_bytes: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoFrameReference {
    pub source_video_id: Uuid,
    pub source_digest: String,
    pub stream_index: u32,
    pub frame_index: String,
    pub pts_us: Option<String>,
    pub timestamp_ms: Option<String>,
    pub width: u32,
    pub height: u32,
    pub image_ref: VideoArtifactReference,
}

#[cfg(feature = "execution-rpc")]
impl VideoFrameReference {
    pub fn from_resolved(
        task: Uuid,
        frame: &crate::video_index::ResolvedVideoFrame,
    ) -> io::Result<Self> {
        use dd_protocol::v1::artifact_ref::Owner;
        let reference = &frame.reference;
        if reference.owner != Some(Owner::OwnerTaskId(task.to_string())) {
            return Err(invalid());
        }
        let image_ref = VideoArtifactReference {
            artifact_id: Uuid::parse_str(&reference.artifact_id).map_err(|_| invalid())?,
            owner: VideoArtifactOwner::Task(task),
            kind: reference.kind.clone(),
            media_type: reference.media_type.clone(),
            sha256: reference.sha256.clone(),
            size_bytes: reference.size_bytes.to_string(),
            schema_id: reference.schema_id.clone(),
        };
        let frame = frame.frame;
        Ok(Self {
            source_video_id: frame.source_video_id,
            source_digest: crate::task_store::format_digest(&frame.source_digest),
            stream_index: frame.stream_index,
            frame_index: frame.frame_index.to_string(),
            pts_us: frame.pts_us.map(|value| value.to_string()),
            timestamp_ms: frame
                .pts_us
                .and_then(|value| u64::try_from(value / 1000).ok())
                .map(|value| value.to_string()),
            width: frame.width,
            height: frame.height,
            image_ref,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoKeyframe {
    pub frame_ref: VideoFrameReference,
    pub annotation_ref: VideoArtifactReference,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnassignedKeyframe {
    pub target_dataset_id: Uuid,
    pub frame_ref: VideoFrameReference,
    pub annotation_ref: VideoArtifactReference,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTargetContext {
    pub label_schema_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoSegment {
    pub segment_id: Uuid,
    pub source_video_id: Uuid,
    pub source_digest: String,
    pub start_us: String,
    pub end_us: String,
    pub keyframes: Vec<VideoKeyframe>,
    pub target_dataset_id: Uuid,
    pub revision: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoAnnotationManifest {
    pub contract_version: u8,
    pub source_video_id: Uuid,
    pub source_digest: String,
    pub target_contexts: BTreeMap<Uuid, VideoTargetContext>,
    pub segments: Vec<VideoSegment>,
    pub unassigned_keyframe_drafts: Vec<UnassignedKeyframe>,
}

impl VideoAnnotationManifest {
    pub(crate) fn keyframes(
        &self,
    ) -> impl Iterator<Item = (Uuid, &VideoFrameReference, &VideoArtifactReference)> {
        self.unassigned_keyframe_drafts
            .iter()
            .map(|draft| {
                (
                    draft.target_dataset_id,
                    &draft.frame_ref,
                    &draft.annotation_ref,
                )
            })
            .chain(self.segments.iter().flat_map(|segment| {
                segment.keyframes.iter().map(|frame| {
                    (
                        segment.target_dataset_id,
                        &frame.frame_ref,
                        &frame.annotation_ref,
                    )
                })
            }))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoKeyframeArtifact {
    pub contract_version: u8,
    pub source_video_id: Uuid,
    pub source_digest: String,
    pub target_dataset_id: Uuid,
    pub label_schema_digest: String,
    pub frame_ref: VideoFrameReference,
    pub coco_document: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoManifestEntry {
    pub reference: VideoArtifactReference,
    pub relative_path: String,
}

pub(crate) fn work_path(video: Uuid, kind: &str, artifact: Uuid) -> String {
    format!("artifacts/videos/{video}/{kind}/{artifact}.json")
}

pub(crate) fn read_video_work(
    root: &Path,
    video: Uuid,
    source_digest: &str,
    reference: &VideoArtifactReference,
    entry: &VideoManifestEntry,
) -> io::Result<(
    VideoAnnotationManifest,
    Vec<(PathBuf, VideoArtifactReference)>,
)> {
    let path = work_path(video, "manifests", reference.artifact_id);
    if entry.reference != *reference || entry.relative_path != path {
        return Err(invalid());
    }
    let bytes = read_asset_artifact(root, video, "VIDEO_ANNOTATION_MANIFEST", &path, reference)?;
    let manifest: VideoAnnotationManifest =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if manifest.contract_version != 1
        || manifest.source_video_id != video
        || manifest.source_digest != source_digest
    {
        return Err(invalid());
    }
    let mut files = vec![(PathBuf::from(path), reference.clone())];
    for (dataset, frame, annotation) in manifest.keyframes() {
        let draft = read_keyframe(root, video, annotation)?;
        if draft.contract_version != 1
            || draft.source_video_id != video
            || draft.source_digest != source_digest
            || draft.target_dataset_id != dataset
            || draft.frame_ref != *frame
            || draft.frame_ref.source_video_id != video
            || draft.frame_ref.source_digest != source_digest
            || manifest
                .target_contexts
                .get(&dataset)
                .map(|context| context.label_schema_digest.as_str())
                != Some(draft.label_schema_digest.as_str())
        {
            return Err(invalid());
        }
        files.push((
            PathBuf::from(work_path(video, "keyframes", annotation.artifact_id)),
            annotation.clone(),
        ));
    }
    Ok((manifest, files))
}

pub(crate) fn read_keyframe(
    root: &Path,
    video: Uuid,
    reference: &VideoArtifactReference,
) -> io::Result<VideoKeyframeArtifact> {
    let path = work_path(video, "keyframes", reference.artifact_id);
    let bytes = read_asset_artifact(root, video, "VIDEO_KEYFRAME_DRAFT", &path, reference)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

fn read_asset_artifact(
    root: &Path,
    video: Uuid,
    kind: &str,
    path: &str,
    reference: &VideoArtifactReference,
) -> io::Result<Vec<u8>> {
    let size = reference.size_bytes.parse::<u64>().map_err(|_| invalid())?;
    if reference.artifact_id.is_nil()
        || reference.owner != VideoArtifactOwner::Asset(video)
        || reference.kind != kind
        || reference.media_type != "application/json"
        || reference.schema_id.is_some()
        || size.to_string() != reference.size_bytes
        || size > VIDEO_WORK_LIMIT
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    open_scoped_file(root, Path::new(path))?
        .take(size + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size || format!("{:x}", Sha256::digest(&bytes)) != reference.sha256 {
        return Err(invalid());
    }
    Ok(bytes)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Video work artifact does not match its durable binding",
    )
}
