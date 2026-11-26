use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    DecimalU64, UuidText,
    dataset_export::CocoExportView,
    workspace::{CommandContext, WorkspaceBackupView},
};

#[derive(Deserialize, TS)]
pub enum ImportFormat {
    #[serde(rename = "COCO_IMAGE_BBOX")]
    CocoImageBbox,
}

#[derive(Deserialize, TS)]
pub struct DatasetImportInspectRequest {
    pub context: CommandContext,
    pub root_grant_id: UuidText,
    pub dataset_name: String,
    pub format: ImportFormat,
}

#[derive(Deserialize, TS)]
pub struct DatasetImportExternalRequest {
    pub context: CommandContext,
    pub import_plan_ref: UuidText,
    pub plan_fingerprint: String,
}

#[derive(Deserialize, TS)]
pub struct TaskGetRequest {
    pub context: CommandContext,
    pub task_id: UuidText,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImportOperationKind {
    DatasetImportInspect,
    DatasetImportExternal,
    AnnotationGeometry,
    VideoProbe,
    ReviewCommit,
    DatasetExport,
    WorkspaceBackup,
    FrameResolve,
    VideoSample,
    StagingCommit,
}

#[derive(Debug, Serialize, TS)]
pub struct ImportOperationRef {
    pub operation_kind: ImportOperationKind,
    pub task_id: UuidText,
    pub status: String,
    pub accepted_at: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ImportFilePreview {
    pub image_path: String,
    pub media_sha256: Option<String>,
    pub annotation_state: String,
    pub annotation_path: Option<String>,
    pub annotation_sha256: Option<String>,
    pub issue_code: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct DatasetImportPreview {
    pub plan_id: UuidText,
    pub plan_fingerprint: String,
    pub dataset_name: String,
    pub category_names: Vec<String>,
    pub files: Vec<ImportFilePreview>,
    pub can_confirm: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct DatasetImportRegistration {
    pub dataset_id: UuidText,
    pub asset_count: DecimalU64,
}

#[derive(Debug, Serialize, TS)]
pub struct ImportTaskView {
    pub task_id: UuidText,
    pub operation_kind: ImportOperationKind,
    pub status: String,
    pub error_code: Option<String>,
    pub preview: Option<DatasetImportPreview>,
    pub registration: Option<DatasetImportRegistration>,
    pub geometry_report: Option<GeometryReportView>,
    pub coco_export: Option<CocoExportView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_backup: Option<WorkspaceBackupView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub video_frame: Option<crate::video::VideoFrame>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub video_sample: Option<crate::video::VideoSampleView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub staging_commit: Option<crate::staging::StagingCommitView>,
}

#[derive(Deserialize, TS)]
pub struct GeometrySubmitRequest {
    pub context: CommandContext,
    pub dataset_id: UuidText,
    pub asset_id: UuidText,
}

#[derive(Debug, Serialize, TS)]
pub struct GeometryReportView {
    pub status: String,
    pub width: u32,
    pub height: u32,
    pub image_digest: String,
    pub findings: Vec<GeometryFindingView>,
}

#[derive(Debug, Serialize, TS)]
pub struct GeometryFindingView {
    pub code: String,
    pub annotation_id: Option<DecimalU64>,
}
