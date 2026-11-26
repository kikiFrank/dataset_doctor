//! Bind a sealed decoder observation to the Core display-frame index.

use std::error::Error;
use std::io::Read;
use std::path::Path;

use dd_domain::video::{DecodedFrame, FrameIndex, Rotation, SourceStream, TimeBase};
use dd_protocol::v1::ArtifactRef;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::artifact_manifest::{TaskManifestScope, verify_task_result_manifest};
use crate::json::JsonContracts;
use crate::paths::open_scoped_file;
use crate::task_store::format_digest;

const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FRAMES: usize = 100_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VideoIndexDocument {
    source_video_id: Uuid,
    source_digest: String,
    stream_index: u32,
    time_base: Option<[u32; 2]>,
    rotation_ccw: u16,
    frames: Vec<ObservedFrame>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedFrame {
    raw_pts: Option<String>,
    width: u32,
    height: u32,
}

pub fn read_video_index(
    approved_attempt_directory: &Path,
    reference: &ArtifactRef,
    scope: &TaskManifestScope,
    video_id: Uuid,
    source_digest: [u8; 32],
    expected_stream_index: Option<u32>,
) -> Result<FrameIndex, Box<dyn Error + Send + Sync>> {
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
    let mut bytes = Vec::new();
    open_scoped_file(&directory, Path::new(&entry.relative_path))?
        .take(entry.size_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != entry.size_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256
    {
        return Err("Video index artifact changed during reading".into());
    }
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
    Ok(FrameIndex::from_display_order(
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
    )?)
}
