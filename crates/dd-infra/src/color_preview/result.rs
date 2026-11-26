use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    CAPABILITY, ColorOptions, ColorPreviewError, ColorRegion, MAX_IMAGE_BYTES, MAX_INPUT_BYTES,
    MAX_PIXELS, PreviewConfig, StoredPreview, input_path, read_scoped,
};
use crate::artifact_manifest::{
    MANIFEST_FILE, TaskArtifactEntry, TaskManifestScope, parse_stored_manifest_reference,
    verify_task_result_manifest,
};
use crate::artifact_reference::{ArtifactOwner, ArtifactReference};
use crate::input_fingerprint::freeze_input_manifest;
use crate::json::{JsonContracts, JsonProfile, strict_json};
use crate::task_store::{format_digest, parse_digest};

#[derive(Debug)]
pub struct VerifiedColorPreview {
    pub task_id: Uuid,
    pub dataset_id: Uuid,
    pub asset_id: Uuid,
    pub asset_revision: u64,
    pub base_image_digest: String,
    pub base_annotation_digest: Option<String>,
    pub label_schema_digest: String,
    pub candidate_ref: ArtifactReference,
    pub mask_ref: ArtifactReference,
    pub candidate_path: PathBuf,
    pub mask_path: PathBuf,
    pub region: ColorRegion,
    pub options: ColorOptions,
    pub changed_pixels: u64,
    pub lossy_reencode: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewDocument {
    asset_id: Uuid,
    base_image_digest: String,
    base_annotation_digest: Option<String>,
    candidate_artifact_id: Uuid,
    mask_artifact_id: Uuid,
    region: ColorRegion,
    operation: ColorOptions,
    changed_pixels: u64,
    lossy_reencode: bool,
}

pub(super) fn read_preview(
    task: Uuid,
    record: &StoredPreview,
    contracts: &JsonContracts,
) -> Result<VerifiedColorPreview, ColorPreviewError> {
    let invalid = || ColorPreviewError::ResultUnconfirmed;
    let bytes = read_scoped(&record.root, &record.input_path, MAX_INPUT_BYTES)?;
    let input = contracts
        .validate(JsonProfile::InputManifest, &bytes)
        .map_err(|_| invalid())?;
    let canonical =
        freeze_input_manifest(contracts, input.clone()).map_err(ColorPreviewError::Fingerprint)?;
    if bytes != canonical.bytes
        || record.input_path != input_path(&canonical)
        || record.input_fingerprint != format_digest(&canonical.input_fingerprint)
        || input["dataset_id"] != record.dataset.to_string()
        || input["capability_id"] != CAPABILITY
    {
        return Err(invalid());
    }
    let [asset] = input["assets"].as_array().ok_or_else(invalid)?.as_slice() else {
        return Err(invalid());
    };
    let config: PreviewConfig =
        serde_json::from_value(input["resolved_config"].clone()).map_err(|_| invalid())?;
    let revision = config
        .asset_revision
        .parse::<u64>()
        .map_err(|_| invalid())?;
    let label_schema = input["label_schema_digest"].as_str().ok_or_else(invalid)?;
    if revision.to_string() != config.asset_revision
        || parse_digest(label_schema).is_none()
        || !config.region.valid()
        || !config.operation.valid()
        || config.max_pixels != MAX_PIXELS
    {
        return Err(invalid());
    }
    let attempt =
        Uuid::parse_str(record.attempt.as_deref().ok_or_else(invalid)?).map_err(|_| invalid())?;
    let relative = PathBuf::from(format!("artifacts/tasks/{task}/{attempt}"));
    if record.output.as_deref().map(Path::new) != Some(relative.join(MANIFEST_FILE).as_path()) {
        return Err(invalid());
    }
    let directory = record.root.join(&relative);
    if directory.canonicalize()? != directory {
        return Err(invalid());
    }
    let seal =
        parse_stored_manifest_reference(record.reference.as_deref().ok_or_else(invalid)?, task)
            .map_err(|error| ColorPreviewError::TaskEvidence(Box::new(error)))?;
    let manifest = verify_task_result_manifest(
        &directory,
        contracts,
        &TaskManifestScope {
            task,
            attempt,
            input_fingerprint: canonical.input_fingerprint,
        },
        &seal,
    )
    .map_err(|error| ColorPreviewError::TaskEvidence(Box::new(error)))?;
    let entries = manifest.entries();
    if entries.len() != 3 {
        return Err(invalid());
    }
    let candidate = entries
        .iter()
        .find(|entry| entry.kind == "REPAIR_CANDIDATE")
        .ok_or_else(invalid)?;
    let mask = entries
        .iter()
        .find(|entry| entry.kind == "REPAIR_MASK")
        .ok_or_else(invalid)?;
    let detail = entries
        .iter()
        .find(|entry| entry.kind == "REPAIR_PREVIEW")
        .ok_or_else(invalid)?;
    if !matches!(
        (
            candidate.relative_path.as_str(),
            candidate.media_type.as_str()
        ),
        ("candidate.png", "image/png") | ("candidate.jpg", "image/jpeg")
    ) || mask.relative_path != "mask.png"
        || mask.media_type != "image/png"
        || detail.relative_path != "color-preview.json"
        || detail.media_type != "application/json"
        || candidate.size_bytes > MAX_IMAGE_BYTES
        || mask.size_bytes > MAX_IMAGE_BYTES
        || detail.size_bytes > MAX_INPUT_BYTES
    {
        return Err(invalid());
    }
    let bytes = read_scoped(
        &directory,
        Path::new(&detail.relative_path),
        detail.size_bytes,
    )?;
    if bytes.len() as u64 != detail.size_bytes
        || format!("{:x}", Sha256::digest(&bytes)) != detail.sha256
    {
        return Err(invalid());
    }
    let document =
        strict_json(&bytes).map_err(|error| ColorPreviewError::TaskEvidence(Box::new(error)))?;
    let data: PreviewDocument = serde_json::from_value(document).map_err(|_| invalid())?;
    if asset["asset_id"] != data.asset_id.to_string()
        || asset["media_digest"] != data.base_image_digest
        || asset["annotation_digest"].as_str() != data.base_annotation_digest.as_deref()
        || candidate.artifact_id != data.candidate_artifact_id.to_string()
        || mask.artifact_id != data.mask_artifact_id.to_string()
        || data.region != config.region
        || data.operation != config.operation
        || data.changed_pixels > u64::from(MAX_PIXELS)
        || (data.changed_pixels == 0 && candidate.sha256 != data.base_image_digest)
        || data.lossy_reencode != (data.changed_pixels > 0 && candidate.media_type == "image/jpeg")
    {
        return Err(invalid());
    }
    Ok(VerifiedColorPreview {
        task_id: task,
        dataset_id: record.dataset,
        asset_id: data.asset_id,
        asset_revision: revision,
        base_image_digest: data.base_image_digest,
        base_annotation_digest: data.base_annotation_digest,
        label_schema_digest: label_schema.to_owned(),
        candidate_ref: artifact_reference(task, candidate, data.candidate_artifact_id),
        mask_ref: artifact_reference(task, mask, data.mask_artifact_id),
        candidate_path: relative.join(&candidate.relative_path),
        mask_path: relative.join(&mask.relative_path),
        region: data.region,
        options: data.operation,
        changed_pixels: data.changed_pixels,
        lossy_reencode: data.lossy_reencode,
    })
}

fn artifact_reference(task: Uuid, entry: &TaskArtifactEntry, artifact: Uuid) -> ArtifactReference {
    ArtifactReference {
        artifact_id: artifact,
        owner: ArtifactOwner::Task(task),
        kind: entry.kind.clone(),
        media_type: entry.media_type.clone(),
        sha256: entry.sha256.clone(),
        size_bytes: entry.size_bytes.to_string(),
        schema_id: entry.schema_id.clone(),
    }
}
