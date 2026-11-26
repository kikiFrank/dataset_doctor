//! Dataset Doctor infrastructure adapters.

#[cfg(feature = "execution-rpc")]
pub mod agent_process;
pub mod annotation_view;
pub mod artifact_reference;
pub mod backup_artifacts;
pub mod backup_assets;
pub mod dataset_import;
#[cfg(feature = "execution-rpc")]
pub mod dataset_profile_runtime;
pub mod dataset_query;
#[cfg(feature = "execution-rpc")]
mod local_process;

#[cfg(feature = "execution-rpc")]
pub mod artifact_manifest;
#[cfg(feature = "execution-rpc")]
pub mod coco_export;
#[cfg(feature = "execution-rpc")]
pub mod color_apply;
#[cfg(feature = "execution-rpc")]
pub mod color_preview;
#[cfg(feature = "execution-rpc")]
pub mod execution_rpc;
#[cfg(feature = "execution-rpc")]
pub mod frame_runtime;
#[cfg(feature = "execution-rpc")]
pub mod geometry_runtime;
#[cfg(feature = "execution-rpc")]
pub mod image_quality_runtime;
pub mod import_preflight;
pub mod import_registration;
pub mod input_fingerprint;
pub mod json;
pub mod media_display;
mod paths;
#[cfg(feature = "execution-rpc")]
pub mod review_draft;
#[cfg(feature = "execution-rpc")]
pub mod review_final;
pub mod review_store;
pub mod review_validation;
pub mod sqlite;
pub mod staging;
#[cfg(feature = "execution-rpc")]
pub mod staging_commit;
#[cfg(feature = "execution-rpc")]
pub mod staging_report;
pub mod task_store;
mod track_map;
pub mod video_annotation_store;
#[cfg(feature = "execution-rpc")]
pub mod video_index;
#[cfg(feature = "execution-rpc")]
pub mod video_keyframes;
#[cfg(feature = "execution-rpc")]
pub mod video_query;
pub mod video_registration;
#[cfg(feature = "execution-rpc")]
pub mod video_runtime;
#[cfg(feature = "execution-rpc")]
pub mod video_tracking_runtime;
#[cfg(feature = "execution-rpc")]
pub mod worker_process;
#[cfg(feature = "execution-rpc")]
mod worker_runtime;
pub mod workflow_store;
pub mod workspace;
pub mod workspace_backup;
pub mod workspace_thread;
