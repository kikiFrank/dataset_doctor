//! Bind a sealed decoder observation to the Core display-frame index.

use std::collections::BTreeSet;
use std::error::Error;
use std::io::Read;
use std::path::{Path, PathBuf};

use dd_domain::video::{DecodedFrame, FrameIndex, FrameRef, Rotation, SourceStream, TimeBase};
use dd_protocol::v1::{ArtifactRef, artifact_ref::Owner};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_manifest::{TaskArtifactEntry, TaskManifestScope, verify_task_result_manifest};
use crate::json::JsonContracts;
use crate::paths::open_scoped_file;
use crate::task_store::format_digest;

const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FRAMES: usize = 100_000;
const MAX_STREAMS: usize = 64;

pub struct VideoProbeObservation {
    pub index: FrameIndex,
    pub metadata: Option<VideoMetadata>,
    pub rotation_ccw: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMetadata {
    pub duration_ms: Option<u64>,
    pub streams: Vec<VideoStreamMetadata>,
    pub variable_frame_rate: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoStreamMetadata {
    pub stream_index: u32,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VideoIndexDocument {
    source_video_id: Uuid,
    source_digest: String,
    stream_index: u32,
    time_base: Option<[u32; 2]>,
    rotation_ccw: u16,
    frames: Vec<ObservedFrame>,
    #[serde(default)]
    metadata: Option<VideoMetadata>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedFrame {
    raw_pts: Option<String>,
    width: u32,
    height: u32,
}

pub struct ResolvedVideoFrame {
    pub frame: FrameRef,
    pub reference: ArtifactRef,
    pub relative_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolvedFrameDocument {
    source_video_id: Uuid,
    source_digest: String,
    stream_index: u32,
    frame_index: String,
    raw_pts: Option<String>,
    width: u32,
    height: u32,
    image_artifact_id: Uuid,
}

pub fn read_video_index(
    approved_attempt_directory: &Path,
    reference: &ArtifactRef,
    scope: &TaskManifestScope,
    video_id: Uuid,
    source_digest: [u8; 32],
    expected_stream_index: Option<u32>,
) -> Result<FrameIndex, Box<dyn Error + Send + Sync>> {
    read_video_observation(
        approved_attempt_directory,
        reference,
        scope,
        video_id,
        source_digest,
        expected_stream_index,
    )
    .map(|observation| observation.index)
}

pub fn read_video_observation(
    approved_attempt_directory: &Path,
    reference: &ArtifactRef,
    scope: &TaskManifestScope,
    video_id: Uuid,
    source_digest: [u8; 32],
    expected_stream_index: Option<u32>,
) -> Result<VideoProbeObservation, Box<dyn Error + Send + Sync>> {
    let directory = approved_attempt_directory.canonicalize()?;
    let verified =
        verify_task_result_manifest(&directory, &JsonContracts::new()?, scope, reference)?;
    let [entry] = verified.entries() else {
        return Err("Video probe must seal exactly one observation".into());
    };
    if entry.kind != "VIDEO_FRAME_INDEX"
        || entry.media_type != "application/json"
        || entry.size_bytes > MAX_INDEX_BYTES
    {
        return Err("Invalid video probe artifact".into());
    }
    let bytes = read_entry(&directory, entry)?;
    let document: VideoIndexDocument = serde_json::from_slice(&bytes)?;
    if document.source_video_id != video_id
        || document.source_digest != format_digest(&source_digest)
        || expected_stream_index.is_some_and(|requested| requested != document.stream_index)
        || document.frames.len() > MAX_FRAMES
    {
        return Err("Video index does not match the registered source and limits".into());
    }
    let rotation = match document.rotation_ccw {
        0 => Rotation::None,
        90 => Rotation::Clockwise270,
        180 => Rotation::Clockwise180,
        270 => Rotation::Clockwise90,
        _ => return Err("Unsupported video display rotation".into()),
    };
    let mut decoded = Vec::with_capacity(document.frames.len());
    for item in document.frames {
        let pts = item
            .raw_pts
            .map(|raw| -> Result<i64, Box<dyn Error + Send + Sync>> {
                let value: i64 = raw.parse()?;
                if value.to_string() != raw {
                    return Err("Noncanonical raw video timestamp".into());
                }
                Ok(value)
            })
            .transpose()?;
        decoded.push(DecodedFrame {
            pts,
            width: item.width,
            height: item.height,
        });
    }
    if let Some(metadata) = &document.metadata {
        let mut indices = BTreeSet::new();
        if metadata.streams.len() > MAX_STREAMS
            || metadata.streams.iter().any(|stream| {
                !indices.insert(stream.stream_index)
                    || stream.width == Some(0)
                    || stream.height == Some(0)
                    || stream
                        .codec
                        .as_ref()
                        .is_some_and(|codec| codec.is_empty() || codec.len() > 128)
            })
            || !indices.contains(&document.stream_index)
        {
            return Err("Video metadata has an invalid stream inventory".into());
        }
    }
    let index = FrameIndex::from_display_order(
        SourceStream {
            video_id,
            source_digest,
            stream_index: document.stream_index,
            rotation,
            time_base: document.time_base.map(|[numerator, denominator]| TimeBase {
                numerator,
                denominator,
            }),
        },
        decoded,
    )?;
    Ok(VideoProbeObservation {
        index,
        metadata: document.metadata,
        rotation_ccw: document.rotation_ccw,
    })
}

pub fn read_resolved_video_frame(
    approved_attempt_directory: &Path,
    reference: &ArtifactRef,
    scope: &TaskManifestScope,
    frame: FrameRef,
) -> Result<ResolvedVideoFrame, Box<dyn Error + Send + Sync>> {
    let directory = approved_attempt_directory.canonicalize()?;
    let verified =
        verify_task_result_manifest(&directory, &JsonContracts::new()?, scope, reference)?;
    let [first, second] = verified.entries() else {
        return Err("Video resolve must seal a frame document and a PNG image".into());
    };
    let (details, image) = if first.kind == "VIDEO_FRAME_REF" {
        (first, second)
    } else {
        (second, first)
    };
    if details.kind != "VIDEO_FRAME_REF"
        || details.media_type != "application/json"
        || details.size_bytes > 4096
        || image.kind != "VIDEO_FRAME_IMAGE"
        || image.media_type != "image/png"
    {
        return Err("Invalid video resolve artifacts".into());
    }
    let document: ResolvedFrameDocument =
        serde_json::from_slice(&read_entry(&directory, details)?)?;
    if document.source_video_id != frame.source_video_id
        || document.source_digest != format_digest(&frame.source_digest)
        || document.stream_index != frame.stream_index
        || document.frame_index != frame.frame_index.to_string()
        || document.raw_pts != frame.raw_pts.map(|pts| pts.to_string())
        || (document.width, document.height) != (frame.width, frame.height)
        || document.image_artifact_id.to_string() != image.artifact_id
    {
        return Err("Resolved video image does not match the requested frame".into());
    }
    let relative_path = PathBuf::from(&image.relative_path);
    let mut header = [0; 24];
    open_scoped_file(&directory, &relative_path)?.read_exact(&mut header)?;
    if header[..8] != *b"\x89PNG\r\n\x1a\n"
        || header[12..16] != *b"IHDR"
        || header[16..20] != frame.width.to_be_bytes()
        || header[20..24] != frame.height.to_be_bytes()
    {
        return Err("Video PNG dimensions differ from the display frame".into());
    }
    Ok(ResolvedVideoFrame {
        frame,
        relative_path,
        reference: ArtifactRef {
            artifact_id: image.artifact_id.clone(),
            owner: Some(Owner::OwnerTaskId(scope.task.to_string())),
            kind: image.kind.clone(),
            media_type: image.media_type.clone(),
            sha256: image.sha256.clone(),
            size_bytes: image.size_bytes,
            schema_id: image.schema_id.clone(),
        },
    })
}

fn read_entry(
    directory: &Path,
    entry: &TaskArtifactEntry,
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let mut bytes = Vec::new();
    open_scoped_file(directory, Path::new(&entry.relative_path))?
        .take(entry.size_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != entry.size_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
    {
        return Err("Video artifact changed during reading".into());
    }
    Ok(bytes)
}
