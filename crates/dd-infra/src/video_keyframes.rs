//! Core publication of human video keyframes with optimistic Asset revisions.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dd_domain::video::FrameIndex;
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::frame_runtime::FrameService;
use crate::import_preflight::LocalCategory;
use crate::json::{JsonContracts, strict_json};
use crate::paths::publish_immutable;
use crate::review_validation::{ReviewImage, validate_review_document};
use crate::task_store::TaskStoreError;
use crate::video_annotation_store::{
    UnassignedKeyframe, VIDEO_WORK_LIMIT, VideoAnnotationManifest, VideoArtifactOwner,
    VideoArtifactReference, VideoFrameReference, VideoKeyframeArtifact, VideoManifestEntry,
    VideoTargetContext, read_keyframe, read_video_work, work_path,
};
use crate::video_query::{VideoSourceAvailability, registered_video_detail};
use crate::video_runtime::read_registered_video_index;
use crate::workspace_thread::{WorkspaceThread, WorkspaceThreadError};

mod sample_task;
mod sampling;
mod segments;
mod staging;
mod tracking_samples;
pub use sample_task::{
    VideoSampleJob, VideoSampleReceipt, VideoSampleService, VideoSampleSnapshot,
    VideoSampleSubmission,
};
pub use sampling::{
    ResolvedVideoSampling, VideoSamplingMode, VideoSamplingPurpose, VideoSamplingRequest,
};
pub use segments::{SegmentReceipt, SegmentRequest};
pub use staging::StagedVideoSample;
pub use tracking_samples::StagingTrackCandidates;

#[derive(Clone)]
pub struct KeyframeDraftRequest {
    pub source_video_id: Uuid,
    pub target_dataset_id: Uuid,
    pub frame_ref: VideoFrameReference,
    pub coco_document: Vec<u8>,
    pub expected_video_revision: u64,
}

#[derive(Debug)]
pub struct KeyframeDraftReceipt {
    pub source_video_id: Uuid,
    pub target_dataset_id: Uuid,
    pub frame_ref: VideoFrameReference,
    pub annotation_ref: VideoArtifactReference,
    pub draft_digest: String,
    pub video_revision: u64,
}

pub struct VideoAnnotationState {
    pub video_revision: u64,
    pub manifest: Option<VideoAnnotationManifest>,
    pub manifest_ref: Option<VideoArtifactReference>,
}

#[derive(Debug)]
pub enum VideoAnnotationError {
    Storage(WorkspaceThreadError),
    Io(io::Error),
    InvalidInput,
    RevisionConflict,
    TrackingEvidenceInvalid,
    Evidence(Box<dyn Error + Send + Sync>),
}
impl Display for VideoAnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(e) => Display::fmt(e, f),
            Self::Io(e) => Display::fmt(e, f),
            Self::InvalidInput => f.write_str("Invalid video keyframe or target context"),
            Self::RevisionConflict => f.write_str("Video work or Dataset categories changed"),
            Self::TrackingEvidenceInvalid => {
                f.write_str("Tracking evidence is unavailable or has a different scope")
            }
            Self::Evidence(e) => Display::fmt(e, f),
        }
    }
}
impl Error for VideoAnnotationError {}
impl From<WorkspaceThreadError> for VideoAnnotationError {
    fn from(e: WorkspaceThreadError) -> Self {
        Self::Storage(e)
    }
}
impl From<io::Error> for VideoAnnotationError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub struct VideoAnnotationService {
    owner: Arc<WorkspaceThread>,
    contracts: JsonContracts,
}

struct Snapshot {
    root: PathBuf,
    revision: u64,
    digest: String,
    metadata: Value,
    encoded_metadata: String,
}

impl VideoAnnotationService {
    fn online_video_index(
        &self,
        session: Uuid,
        video: Uuid,
        digest: &str,
    ) -> Result<FrameIndex, VideoAnnotationError> {
        let detail = registered_video_detail(&self.owner, session, video)
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))?;
        if detail.source_availability != VideoSourceAvailability::Online
            || detail.source_digest != digest
            || detail.probe_status.as_deref() != Some("SUCCEEDED")
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let probe = detail
            .probe_receipt
            .ok_or(VideoAnnotationError::InvalidInput)?;
        read_registered_video_index(&self.owner, session, probe)
            .map_err(|error| VideoAnnotationError::Evidence(Box::new(error)))
    }

    pub fn new(owner: Arc<WorkspaceThread>) -> Result<Self, VideoAnnotationError> {
        Ok(Self {
            owner,
            contracts: JsonContracts::new().map_err(VideoAnnotationError::Evidence)?,
        })
    }

    pub fn read(
        &self,
        session: Uuid,
        video: Uuid,
    ) -> Result<VideoAnnotationState, VideoAnnotationError> {
        let snapshot = self.snapshot(session, video)?;
        let (manifest, reference) = self.load_manifest(&snapshot, video)?;
        if let Some(manifest) = &manifest {
            for (_, frame, _) in manifest.keyframes() {
                self.verify_frame(session, frame)?;
            }
        }
        Ok(VideoAnnotationState {
            video_revision: snapshot.revision,
            manifest,
            manifest_ref: reference,
        })
    }

    pub fn load_draft(
        &self,
        session: Uuid,
        video: Uuid,
        reference: VideoArtifactReference,
    ) -> Result<VideoKeyframeArtifact, VideoAnnotationError> {
        let snapshot = self.snapshot(session, video)?;
        let (manifest, _) = self.load_manifest(&snapshot, video)?;
        let manifest = manifest.ok_or(VideoAnnotationError::InvalidInput)?;
        if !manifest
            .keyframes()
            .any(|(_, _, annotation)| *annotation == reference)
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let draft = read_keyframe(&snapshot.root, video, &reference)?;
        self.verify_frame(session, &draft.frame_ref)?;
        Ok(draft)
    }

    pub fn save(
        &self,
        session: Uuid,
        request: KeyframeDraftRequest,
        now_ms: i64,
    ) -> Result<KeyframeDraftReceipt, VideoAnnotationError> {
        let _guard = self.owner.lock_final_operations()?;
        let snapshot = self.snapshot(session, request.source_video_id)?;
        if snapshot.revision != request.expected_video_revision {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        let detail = registered_video_detail(&self.owner, session, request.source_video_id)
            .map_err(|e| VideoAnnotationError::Evidence(Box::new(e)))?;
        if detail.source_availability != VideoSourceAvailability::Online
            || detail.source_digest != snapshot.digest
            || request.frame_ref.source_video_id != request.source_video_id
            || request.frame_ref.source_digest != snapshot.digest
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        self.verify_frame(session, &request.frame_ref)?;
        let dataset = request.target_dataset_id;
        let categories = self.owner.workspace_action(session, move |workspace| {
            workspace
                .connection
                .query_row(
                    "SELECT categories_json FROM datasets WHERE dataset_id=?1",
                    [dataset.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .map_err(database)
        })?;
        let schema_digest = digest(categories.as_bytes());
        let (manifest, _) = self.load_manifest(&snapshot, request.source_video_id)?;
        let mut manifest = manifest.unwrap_or_else(|| VideoAnnotationManifest {
            contract_version: 1,
            source_video_id: request.source_video_id,
            source_digest: snapshot.digest.clone(),
            target_contexts: BTreeMap::new(),
            segments: Vec::new(),
            unassigned_keyframe_drafts: Vec::new(),
        });
        if manifest
            .target_contexts
            .get(&dataset)
            .is_some_and(|context| context.label_schema_digest != schema_digest)
        {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        if request.coco_document.len() as u64 > VIDEO_WORK_LIMIT {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let input =
            strict_json(&request.coco_document).map_err(|_| VideoAnnotationError::InvalidInput)?;
        let filename = input["images"][0]["file_name"]
            .as_str()
            .ok_or(VideoAnnotationError::InvalidInput)?;
        if filename.is_empty()
            || filename.contains(['/', '\\', ':'])
            || matches!(filename, "." | "..")
        {
            return Err(VideoAnnotationError::InvalidInput);
        }
        let labels: Vec<LocalCategory> =
            serde_json::from_str(&categories).map_err(|_| VideoAnnotationError::InvalidInput)?;
        let coco = validate_review_document(
            &self.contracts,
            &request.coco_document,
            ReviewImage {
                file_name: filename,
                width: request.frame_ref.width,
                height: request.frame_ref.height,
            },
            &labels,
        )
        .map_err(|e| VideoAnnotationError::Evidence(Box::new(e)))?
        .into_coco();
        let draft = VideoKeyframeArtifact {
            contract_version: 1,
            source_video_id: request.source_video_id,
            source_digest: snapshot.digest.clone(),
            target_dataset_id: dataset,
            label_schema_digest: schema_digest.clone(),
            frame_ref: request.frame_ref.clone(),
            coco_document: coco,
        };
        let (annotation_ref, _) = publish(
            &snapshot.root,
            request.source_video_id,
            "keyframes",
            "VIDEO_KEYFRAME_DRAFT",
            &draft,
        )?;
        manifest.target_contexts.insert(
            dataset,
            VideoTargetContext {
                label_schema_digest: schema_digest,
            },
        );
        manifest.unassigned_keyframe_drafts.retain(|previous| {
            previous.target_dataset_id != dataset || previous.frame_ref != request.frame_ref
        });
        manifest
            .unassigned_keyframe_drafts
            .push(UnassignedKeyframe {
                target_dataset_id: dataset,
                frame_ref: request.frame_ref.clone(),
                annotation_ref: annotation_ref.clone(),
            });
        let (_, next) = self.commit_manifest(
            session,
            request.source_video_id,
            snapshot,
            &manifest,
            (dataset, categories),
            now_ms,
        )?;
        Ok(KeyframeDraftReceipt {
            source_video_id: request.source_video_id,
            target_dataset_id: dataset,
            frame_ref: request.frame_ref,
            draft_digest: annotation_ref.sha256.clone(),
            annotation_ref,
            video_revision: next,
        })
    }

    fn commit_manifest(
        &self,
        session: Uuid,
        video: Uuid,
        snapshot: Snapshot,
        manifest: &VideoAnnotationManifest,
        target: (Uuid, String),
        now_ms: i64,
    ) -> Result<(VideoArtifactReference, u64), VideoAnnotationError> {
        let (dataset, categories) = target;
        let next = snapshot
            .revision
            .checked_add(1)
            .filter(|value| i64::try_from(*value).is_ok())
            .ok_or(VideoAnnotationError::InvalidInput)?;
        let (manifest_ref, path) = publish(
            &snapshot.root,
            video,
            "manifests",
            "VIDEO_ANNOTATION_MANIFEST",
            &manifest,
        )?;
        let mut metadata = snapshot.metadata.clone();
        metadata["video_annotation_manifest_ref"] = json!(manifest_ref);
        metadata["video_annotation_manifest_entry"] = json!(VideoManifestEntry {
            reference: manifest_ref.clone(),
            relative_path: path
        });
        let changed=self.owner.workspace_action(session,move|workspace|{
            workspace.connection.execute("UPDATE assets SET metadata_json=?1,row_revision=?2,updated_at_ms=?3
                WHERE asset_id=?4 AND kind='VIDEO' AND row_revision=?5 AND media_digest=?6 AND metadata_json=?7
                AND EXISTS(SELECT 1 FROM datasets WHERE dataset_id=?8 AND categories_json=?9)",
                params![metadata.to_string(),next as i64,now_ms,video.to_string(),snapshot.revision as i64,snapshot.digest,
                    snapshot.encoded_metadata,dataset.to_string(),categories]).map_err(database)
        })?;
        if changed != 1 {
            return Err(VideoAnnotationError::RevisionConflict);
        }
        Ok((manifest_ref, next))
    }

    fn snapshot(&self, session: Uuid, video: Uuid) -> Result<Snapshot, VideoAnnotationError> {
        self.owner.workspace_action(session,move|workspace|{
            workspace.connection.query_row("SELECT row_revision,media_digest,metadata_json FROM assets WHERE asset_id=?1 AND kind='VIDEO'",
                [video.to_string()],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))
                .map_err(database).and_then(|(revision,digest,metadata)|{
                    let revision=u64::try_from(revision).map_err(|_|invalid_storage())?;
                    let encoded_metadata=metadata;
                    let metadata:Value=serde_json::from_str(&encoded_metadata).map_err(|_|invalid_storage())?;
                    if !metadata.is_object(){return Err(invalid_storage());}
                    Ok(Snapshot{root:workspace.root().to_owned(),revision,digest,metadata,encoded_metadata})
                })
        }).map_err(VideoAnnotationError::Storage)
    }

    fn load_manifest(
        &self,
        snapshot: &Snapshot,
        video: Uuid,
    ) -> Result<
        (
            Option<VideoAnnotationManifest>,
            Option<VideoArtifactReference>,
        ),
        VideoAnnotationError,
    > {
        let reference = snapshot.metadata.get("video_annotation_manifest_ref");
        let entry = snapshot.metadata.get("video_annotation_manifest_entry");
        if reference.is_none() && entry.is_none() {
            return Ok((None, None));
        }
        let reference: VideoArtifactReference = serde_json::from_value(
            reference
                .cloned()
                .ok_or(VideoAnnotationError::InvalidInput)?,
        )
        .map_err(|_| VideoAnnotationError::InvalidInput)?;
        let entry: VideoManifestEntry =
            serde_json::from_value(entry.cloned().ok_or(VideoAnnotationError::InvalidInput)?)
                .map_err(|_| VideoAnnotationError::InvalidInput)?;
        let (manifest, _) =
            read_video_work(&snapshot.root, video, &snapshot.digest, &reference, &entry)?;
        Ok((Some(manifest), Some(reference)))
    }

    fn verify_frame(
        &self,
        session: Uuid,
        frame: &VideoFrameReference,
    ) -> Result<(), VideoAnnotationError> {
        let VideoArtifactOwner::Task(task) = frame.image_ref.owner else {
            return Err(VideoAnnotationError::InvalidInput);
        };
        let resolved = FrameService::new(Arc::clone(&self.owner))
            .and_then(|service| service.snapshot(session, task))
            .map_err(|e| VideoAnnotationError::Evidence(Box::new(e)))?
            .frame
            .ok_or(VideoAnnotationError::InvalidInput)?;
        if VideoFrameReference::from_resolved(task, &resolved)? != *frame {
            return Err(VideoAnnotationError::InvalidInput);
        }
        Ok(())
    }
}

fn publish(
    root: &Path,
    video: Uuid,
    directory: &str,
    kind: &str,
    value: &impl serde::Serialize,
) -> Result<(VideoArtifactReference, String), VideoAnnotationError> {
    let bytes = serde_json::to_vec(value).map_err(|_| VideoAnnotationError::InvalidInput)?;
    if bytes.len() as u64 > VIDEO_WORK_LIMIT {
        return Err(VideoAnnotationError::InvalidInput);
    }
    let artifact_id = Uuid::new_v4();
    let path = work_path(video, directory, artifact_id);
    publish_immutable(root, Path::new(&path), &bytes)?;
    Ok((
        VideoArtifactReference {
            artifact_id,
            owner: VideoArtifactOwner::Asset(video),
            kind: kind.into(),
            media_type: "application/json".into(),
            sha256: digest(&bytes),
            size_bytes: bytes.len().to_string(),
            schema_id: None,
        },
        path,
    ))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn database(error: rusqlite::Error) -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::Database(error))
}
fn invalid_storage() -> WorkspaceThreadError {
    WorkspaceThreadError::Task(TaskStoreError::InvalidManifestPath)
}
