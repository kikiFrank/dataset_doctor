#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod workspace_commands;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use dd_infra::workspace_thread::WorkspaceThread;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

struct RuntimeMode {
    smoke_root: Option<PathBuf>,
    smoke_workspace: Option<PathBuf>,
}

#[derive(Deserialize)]
struct DesktopReadyRequest {
    webview2: bool,
}

#[derive(Serialize)]
struct DesktopReadyResponse {
    version: String,
    verify_workspace: bool,
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
    Ok(DesktopReadyResponse {
        version,
        verify_workspace: runtime.smoke_workspace.is_some(),
    })
}

#[tauri::command]
fn desktop_workspace_verified(
    runtime: State<'_, RuntimeMode>,
    app: AppHandle,
) -> Result<(), String> {
    if runtime.smoke_workspace.is_none() {
        return Err("Workspace verification mode is inactive".into());
    }
    finish_smoke(&app, "DESKTOP_WORKSPACE_VERIFIED")
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
    let smoke_workspace = if cfg!(debug_assertions)
        && std::env::args_os().any(|argument| argument == "--workspace-smoke")
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
    let workspace = WorkspaceThread::start(250, workspace_commands::GRANT_LIFETIME)?;
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(workspace))
        .manage(RuntimeMode {
            smoke_root,
            smoke_workspace,
        })
        .setup(|app| {
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
            workspace_commands::workspace_get,
            workspace_commands::workspace_close,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
