use std::sync::Arc;

use dd_infra::dataset_query::{AssetAnnotationState, DatasetQueryError};
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use dd_ui_contracts::dataset::{
    AnnotationState, AssetListRequest, AssetPage, AssetSummary, DatasetListRequest, DatasetPage,
    DatasetSummary,
};
use dd_ui_contracts::workspace::AppError;
use tauri::State;

use super::{Commands, run};

#[tauri::command]
pub async fn dataset_list(
    request: DatasetListRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<DatasetPage, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.list_datasets(request),
    )
    .await
}

#[tauri::command]
pub async fn asset_list(
    request: AssetListRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<AssetPage, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.list_assets(request),
    )
    .await
}

impl Commands {
    pub(super) fn list_datasets(
        &self,
        request: DatasetListRequest,
    ) -> Result<DatasetPage, AppError> {
        let session = self.session(request.context)?;
        let cursor = request
            .page_cursor
            .map(|value| self.parse_uuid(value))
            .transpose()?;
        let page = self
            .thread
            .list_datasets(session, cursor, request.page_size)
            .map_err(|failure| self.dataset_error(failure))?;
        let items = page
            .items
            .into_iter()
            .map(|item| {
                Ok(DatasetSummary {
                    dataset_id: self.uuid_text(item.dataset_id.to_string())?,
                    name: item.name,
                    asset_count: item.asset_count.into(),
                    unconfirmed_count: item.unconfirmed_count.into(),
                    row_revision: item.row_revision.into(),
                    label_schema_digest: item.schema_fingerprint,
                    updated_at_ms: item.updated_at_ms.into(),
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        let next_cursor = page
            .next_cursor
            .map(|id| self.uuid_text(id.to_string()))
            .transpose()?;
        Ok(DatasetPage { items, next_cursor })
    }

    pub(super) fn list_assets(&self, request: AssetListRequest) -> Result<AssetPage, AppError> {
        let session = self.session(request.context)?;
        let dataset = self.parse_uuid(request.dataset_id.clone())?;
        let cursor = request
            .cursor
            .map(|value| self.parse_uuid(value))
            .transpose()?;
        let page = self
            .thread
            .list_assets(session, dataset, cursor, request.page_size)
            .map_err(|failure| self.dataset_error(failure))?;
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let annotation_state = annotation_state(item.annotation_state);
                Ok(AssetSummary {
                    asset_id: self.uuid_text(item.asset_id.to_string())?,
                    dataset_id: request.dataset_id.clone(),
                    display_name: item.filename,
                    kind: item.kind,
                    availability: item.availability,
                    annotation_state,
                    active_review: item.active_review,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        let next_cursor = page
            .next_cursor
            .map(|id| self.uuid_text(id.to_string()))
            .transpose()?;
        Ok(AssetPage { items, next_cursor })
    }

    pub(super) fn dataset_error(&self, failure: WorkspaceThreadError) -> AppError {
        match failure {
            WorkspaceThreadError::Dataset(DatasetQueryError::InvalidPageSize) => {
                self.error("INVALID_ARGUMENT", "每页数量须在 1 至 200 之间。")
            }
            WorkspaceThreadError::Dataset(DatasetQueryError::UnknownDataset) => {
                self.error("NOT_FOUND", "当前工作区中没有这个数据集。")
            }
            WorkspaceThreadError::Dataset(DatasetQueryError::UnknownAsset) => {
                self.error("NOT_FOUND", "当前工作区中没有这个样本。")
            }
            failure => self.map_error(failure),
        }
    }
}

pub(super) fn annotation_state(state: AssetAnnotationState) -> AnnotationState {
    match state {
        AssetAnnotationState::Unannotated => AnnotationState::Unannotated,
        AssetAnnotationState::Unreviewed => AnnotationState::Unreviewed,
        AssetAnnotationState::ConfirmedPositive => AnnotationState::ConfirmedPositive,
        AssetAnnotationState::ConfirmedNegative => AnnotationState::ConfirmedNegative,
        AssetAnnotationState::Stale => AnnotationState::Stale,
    }
}
