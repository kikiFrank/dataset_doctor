#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod workspace_commands;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::media_display::MediaDisplayService;
use dd_infra::workspace_thread::WorkspaceThread;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

struct RuntimeMode {
    smoke_root: Option<PathBuf>,
    smoke_workspace: Option<PathBuf>,
    smoke_workspace_create: bool,
    smoke_business_directories: Option<(PathBuf, PathBuf)>,
    smoke_color: bool,
    smoke_workflow: bool,
    smoke_workflow_controls: bool,
    smoke_report_export: bool,
    smoke_staging_batch: bool,
    smoke_video: Option<PathBuf>,
}

#[derive(Deserialize)]
struct DesktopReadyRequest {
    webview2: bool,
}

#[derive(Serialize)]
struct DesktopReadyResponse {
    version: String,
    verify_workspace: bool,
    verify_workspace_create: bool,
    verify_workspace_business: bool,
    verify_workspace_color: bool,
    verify_workspace_workflow: bool,
    verify_workspace_workflow_controls: bool,
    verify_workspace_report_export: bool,
    verify_workspace_staging_batch: bool,
    verify_workspace_video: bool,
}

#[tauri::command]
fn desktop_ready(
    request: DesktopReadyRequest,
    runtime: State<'_, RuntimeMode>,
    app: AppHandle,
) -> Result<DesktopReadyResponse, String> {
    let version = app.package_info().version.to_string();
    if runtime.smoke_root.is_some() {
        if !request.webview2 {
            app.exit(2);
            return Err("WebView2 was unavailable to the desktop UI".into());
        }
        if runtime.smoke_workspace.is_none() {
            finish_smoke(&app, "DESKTOP_WEBVIEW2_READY")?;
        }
    }
    let business = runtime.smoke_business_directories.is_some();
    Ok(DesktopReadyResponse {
        version,
        verify_workspace: runtime.smoke_workspace.is_some()
            && !runtime.smoke_workspace_create
            && !runtime.smoke_staging_batch
            && runtime.smoke_video.is_none()
            && !business,
        verify_workspace_create: runtime.smoke_workspace_create,
        verify_workspace_business: business,
        verify_workspace_color: runtime.smoke_color,
        verify_workspace_workflow: runtime.smoke_workflow
            || runtime.smoke_workflow_controls
            || runtime.smoke_report_export,
        verify_workspace_workflow_controls: runtime.smoke_workflow_controls,
        verify_workspace_report_export: runtime.smoke_report_export,
        verify_workspace_staging_batch: runtime.smoke_staging_batch,
        verify_workspace_video: runtime.smoke_video.is_some(),
    })
}

#[tauri::command]
async fn desktop_workspace_verified(
    mut failure: Option<String>,
    runtime: State<'_, RuntimeMode>,
    geometry: State<'_, Option<Arc<workspace_commands::geometry::GeometryRunner>>>,
    app: AppHandle,
) -> Result<(), String> {
    if runtime.smoke_workspace.is_none() {
        return Err("Workspace verification mode is inactive".into());
    }
    let business = runtime.smoke_business_directories.is_some();
    if business && let Some(runner) = geometry.inner().clone() {
        // The task receipt can be read while its Worker is still shutting down.
        let stopped = tauri::async_runtime::spawn_blocking(move || {
            runner.wait_for_idle(std::time::Duration::from_secs(12))
        })
        .await
        .map_err(|error| error.to_string())?;
        if !stopped && failure.is_none() {
            failure = Some("Geometry Worker teardown timed out".into());
        }
    }
    if let Some(failure) = failure {
        if !business && !runtime.smoke_staging_batch && runtime.smoke_video.is_none() {
            return Err("Business verification mode is inactive".into());
        }
        eprintln!("DESKTOP_WORKSPACE_BUSINESS_FAILED {failure}");
        std::io::stderr()
            .flush()
            .map_err(|error| error.to_string())?;
        app.exit(2);
        return Ok(());
    }
    let marker = if runtime.smoke_color {
        "DESKTOP_WORKSPACE_COLOR_VERIFIED"
    } else if runtime.smoke_report_export {
        "DESKTOP_WORKSPACE_REPORT_EXPORT_VERIFIED"
    } else if runtime.smoke_workflow_controls {
        "DESKTOP_WORKSPACE_WORKFLOW_CONTROLS_VERIFIED"
    } else if runtime.smoke_workflow {
        "DESKTOP_WORKSPACE_WORKFLOW_VERIFIED"
    } else if runtime.smoke_staging_batch {
        "DESKTOP_WORKSPACE_STAGING_BATCH_VERIFIED"
    } else if runtime.smoke_video.is_some() {
        "DESKTOP_WORKSPACE_VIDEO_VERIFIED"
    } else if business {
        "DESKTOP_WORKSPACE_BUSINESS_VERIFIED"
    } else if runtime.smoke_workspace_create {
        "DESKTOP_WORKSPACE_CREATE_VERIFIED"
    } else {
        "DESKTOP_WORKSPACE_VERIFIED"
    };
    finish_smoke(&app, marker)
}

fn finish_smoke(app: &AppHandle, marker: &str) -> Result<(), String> {
    println!("{marker} version={}", app.package_info().version);
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    app.exit(0);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let smoke_root = if std::env::args_os().any(|argument| argument == "--runtime-smoke") {
        Some(
            PathBuf::from(std::env::var_os("DD_DESKTOP_SMOKE_ROOT").ok_or("missing smoke root")?)
                .canonicalize()?,
        )
    } else {
        None
    };
    let smoke_workspace_create = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-create-smoke");
    let smoke_workspace_business = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-business-smoke");
    let smoke_color = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-color-smoke");
    let smoke_workflow = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-workflow-smoke");
    let smoke_workflow_controls = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-workflow-controls-smoke");
    let smoke_report_export = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-report-export-smoke");
    let smoke_staging_batch = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-staging-batch-smoke");
    let smoke_workspace_video = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-video-smoke");
    let smoke_workspace_open = cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-smoke");
    if [
        smoke_workspace_create,
        smoke_workspace_business,
        smoke_color,
        smoke_workflow,
        smoke_workflow_controls,
        smoke_report_export,
        smoke_staging_batch,
        smoke_workspace_video,
        smoke_workspace_open,
    ]
    .into_iter()
    .filter(|enabled| *enabled)
    .count()
        > 1
    {
        return Err("Choose one Workspace smoke mode".into());
    }
    let smoke_workspace = if smoke_workspace_create
        || smoke_workspace_open
        || smoke_workspace_business
        || smoke_color
        || smoke_workflow
        || smoke_workflow_controls
        || smoke_report_export
        || smoke_staging_batch
        || smoke_workspace_video
    {
        let path = PathBuf::from(
            std::env::var_os("DD_DESKTOP_SMOKE_WORKSPACE").ok_or("missing Workspace fixture")?,
        )
        .canonicalize()?;
        if path.parent() != smoke_root.as_deref() {
            return Err("Workspace fixture must be in the desktop smoke run".into());
        }
        Some(path)
    } else {
        None
    };
    let smoke_video = if smoke_workspace_video {
        let root = smoke_root
            .as_deref()
            .ok_or("Video verification requires a dedicated run")?;
        let expected = root.join("Synthetic motion.mkv");
        let path = expected.canonicalize()?;
        if path != expected || !path.is_file() {
            return Err("The synthetic video must be a direct child of the smoke run".into());
        }
        Some(path)
    } else {
        None
    };
    let smoke_business_directories = if smoke_workspace_business
        || smoke_color
        || smoke_workflow
        || smoke_workflow_controls
        || smoke_report_export
    {
        let root = smoke_root
            .as_deref()
            .ok_or("Business verification requires a dedicated run")?;
        let selected = |name: &str| -> Result<PathBuf, Box<dyn std::error::Error>> {
            let expected = root.join(name);
            let resolved = expected.canonicalize()?;
            if resolved != expected || !resolved.is_dir() {
                return Err(
                    "Business fixture directory must be a direct child of the smoke run".into(),
                );
            }
            Ok(resolved)
        };
        if smoke_workspace.as_ref() != Some(&selected("Workspace")?) {
            return Err("Business verification requires the fixture Workspace directory".into());
        }
        Some((selected("Inputs")?, selected("Exports")?))
    } else {
        None
    };
    let workspace = Arc::new(WorkspaceThread::start(
        250,
        workspace_commands::GRANT_LIFETIME,
    )?);
    let imports = Arc::new(
        DatasetImportCoordinator::new(Arc::clone(&workspace))
            .map_err(|error| std::io::Error::other(error.to_string()))?,
    );
    let media = Arc::new(MediaDisplayService::new(
        Arc::clone(&workspace),
        workspace_commands::GRANT_LIFETIME,
    ));
    tauri::Builder::default()
        .register_uri_scheme_protocol("dd-media", |context, request| {
            let service = context.app_handle().state::<Arc<MediaDisplayService>>();
            workspace_commands::annotation::media_response(&service, request)
        })
        .plugin(tauri_plugin_dialog::init())
        .manage(workspace)
        .manage(media)
        .manage(imports)
        .manage(RuntimeMode {
            smoke_root,
            smoke_workspace,
            smoke_workspace_create,
            smoke_business_directories,
            smoke_color,
            smoke_workflow,
            smoke_workflow_controls,
            smoke_report_export,
            smoke_staging_batch,
            smoke_video,
        })
        .setup(|app| {
            let worker =
                workspace_commands::configured_worker_program(app.path().resource_dir().ok());
            app.manage(workspace_commands::video::VideoRunner::configured(
                worker.clone(),
            ));
            app.manage(workspace_commands::geometry::GeometryRunner::configured(
                worker.clone(),
            ));
            app.manage(workspace_commands::color::ColorRunner::configured(
                worker.clone(),
            ));
            let owner = Arc::clone(app.state::<Arc<WorkspaceThread>>().inner());
            app.manage(workspace_commands::workflow::configured(owner, worker));
            let runtime = app.state::<RuntimeMode>();
            let mut window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("Dataset Doctor")
                    .inner_size(1120.0, 760.0)
                    .min_inner_size(720.0, 480.0);
            if let Some(root) = &runtime.smoke_root {
                window = window.data_directory(root.join("webview")).visible(false);
            }
            window.build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_ready,
            desktop_workspace_verified,
            workspace_commands::path_select,
            workspace_commands::workspace_open,
            workspace_commands::workspace_create,
            workspace_commands::backup::workspace_restore,
            workspace_commands::backup::workspace_backup_submit,
            workspace_commands::backup::workspace_backup_latest,
            workspace_commands::workspace_get,
            workspace_commands::workspace_close,
            workspace_commands::dataset::dataset_list,
            workspace_commands::dataset::dataset_get,
            workspace_commands::dataset::asset_list,
            workspace_commands::annotation::annotation_get,
            workspace_commands::annotation::media_get_locator,
            workspace_commands::dataset_import::dataset_import_inspect,
            workspace_commands::dataset_import::dataset_import_external,
            workspace_commands::dataset_import::task_get,
            workspace_commands::geometry::annotation_geometry_submit,
            workspace_commands::workflow::workflow_start,
            workspace_commands::workflow::workflow_get,
            workspace_commands::workflow::workflow_pause,
            workspace_commands::workflow::workflow_resume,
            workspace_commands::workflow::workflow_complete_review,
            workspace_commands::workflow::workflow_list,
            workspace_commands::color::repair_color_preview,
            workspace_commands::color::repair_color_apply,
            workspace_commands::video::video_import,
            workspace_commands::video::video_get,
            workspace_commands::video_frames::video_resolve_frame,
            workspace_commands::video_annotations::video_save_keyframe_draft,
            workspace_commands::video_annotations::video_get_keyframe_draft,
            workspace_commands::video_annotations::video_save_segment,
            workspace_commands::video_sampling::video_sample_submit,
            workspace_commands::video_tracking::video_track_submit,
            workspace_commands::staging::staging_list,
            workspace_commands::staging::staging_batch_report_build,
            workspace_commands::staging::staging_commit,
            workspace_commands::staging::report_get,
            workspace_commands::report_export::report_export_submit,
            workspace_commands::review::review_open,
            workspace_commands::review::review_save_draft,
            workspace_commands::review::review_cancel,
            workspace_commands::review_commit::review_commit,
            workspace_commands::dataset_export::dataset_export_submit,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
