//! Capture Dataset membership and input proofs for the local default plan.

use std::path::PathBuf;

use dd_core::workflow::{ScopeKind, WorkflowInput};
use serde_json::json;
use uuid::Uuid;

use super::super::{CAPABILITY, WorkflowDraftRequest, WorkflowError, WorkflowStore, database};
use super::{LocalHealthcheckRequest, LocalHealthcheckRuntime, execution};
use crate::input_fingerprint::freeze_input_manifest;
use crate::paths::publish_immutable;
use crate::sqlite::row_uuid;
use crate::task_store::format_digest;
use crate::workspace_thread::WorkspaceThreadError;

pub(super) fn freeze_dataset(
    store: &WorkflowStore,
    session: Uuid,
    request: &LocalHealthcheckRequest,
) -> Result<WorkflowDraftRequest, WorkflowError> {
    let dataset = request.dataset_id;
    let (root, categories, images) =
        store
            .owner
            .workspace_action(session, move |owner| {
                let detail = owner
                    .dataset_detail(dataset)
                    .map_err(WorkspaceThreadError::Dataset)?;
                let mut query = owner.connection.prepare(
            "SELECT a.asset_id FROM dataset_assets m JOIN assets a ON a.asset_id=m.asset_id
             WHERE m.dataset_id=?1 AND a.kind='IMAGE' ORDER BY a.asset_id",
        ).map_err(database)?;
                let assets = query
                    .query_map([dataset.to_string()], |row| row_uuid(row, 0))
                    .map_err(database)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(database)?;
                let images = assets
                    .into_iter()
                    .map(|asset| {
                        owner
                            .registered_image(dataset, asset)
                            .map_err(WorkspaceThreadError::Dataset)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    owner.root().to_owned(),
                    detail.summary.schema_fingerprint,
                    images,
                ))
            })
            .map_err(WorkflowError::Storage)?;
    let assets: Vec<_> = images.iter().map(|image| json!({
        "asset_id":image.asset_id, "media_digest":image.image.sha256,
        "annotation_digest":image.annotation.as_ref().map(|source| &source.sha256), "source_metadata_digest":null,
    })).collect();
    let manifest = freeze_input_manifest(&store.contracts, json!({
        "dataset_id":dataset, "label_schema_digest":categories, "assets":assets,
        "capability_id":CAPABILITY, "implementation_version":env!("CARGO_PKG_VERSION"), "model_identity":null,
        "resolved_config":{"plan_source":"LOCAL_DEFAULT", "sample_limit":request.sample_limit, "max_pixels":request.max_pixels},
    })).map_err(execution)?;
    let path = PathBuf::from(format!(
        "artifacts/task-inputs/{}.json",
        format_digest(&manifest.sha256)
    ));
    publish_immutable(&root, &path, &manifest.bytes).map_err(execution)?;
    let manifest_id = uuid::Builder::from_custom_bytes(
        manifest.sha256[..16].try_into().expect("16 digest bytes"),
    )
    .into_uuid();
    Ok(WorkflowDraftRequest {
        dataset_id: dataset,
        source: WorkflowInput {
            kind: ScopeKind::Dataset,
            manifest_id,
            fingerprint: manifest.input_fingerprint,
        },
        template_id: LocalHealthcheckRuntime::template_id().into(),
        input_path: path,
        input_manifest_sha256: manifest.sha256,
        request_id: request.request_id,
        idempotency_key: request.idempotency_key,
    })
}
