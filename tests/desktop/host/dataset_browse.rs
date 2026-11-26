use std::fs;
use std::path::PathBuf;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::media_display::MediaDisplayService;
use dd_ui_contracts::annotation::AnnotationGetRequest;
use dd_ui_contracts::annotation::{MediaGetLocatorRequest, MediaPurpose};
use dd_ui_contracts::dataset::{AnnotationState, AssetListRequest, DatasetListRequest};
use tauri::http::Request;

use super::*;

#[test]
fn commands_page_only_current_workspace_data_and_report_invalid_arguments() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let allowed = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/desktop-commands/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(allowed.as_path()));
    let root = run.join("browse workspace");
    let source = run.join("browse inputs");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    let image = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
    fs::write(source.join("first.png"), image).unwrap();
    fs::write(source.join("second.png"), image).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, GRANT_LIFETIME).unwrap());
    let grant = thread.grant_selected_directory(root).unwrap();
    let workspace = thread
        .create(grant.grant_id, "目录查询".into(), 12)
        .unwrap();
    let session = workspace.workspace_session_id;
    let importer = DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap();
    let grant = thread.grant_import_directory(session, source).unwrap();
    let plan = importer
        .inspect(
            session,
            grant.grant_id,
            "验收样本".into(),
            vec!["first.png".into(), "second.png".into()],
        )
        .unwrap();
    let registered = importer
        .confirm(session, plan.plan_id, plan.fingerprint, 13)
        .unwrap();
    let id = |value: String| UuidText::try_from(value).unwrap();
    let commands = Commands {
        thread,
        request_id: id(Uuid::new_v4().to_string()),
    };
    let context = || CommandContext {
        request_id: commands.request_id.clone(),
        workspace_id: id(workspace.workspace_id.clone()),
        workspace_session_id: id(session.to_string()),
        idempotency_key: None,
    };
    let result = commands
        .list_datasets(DatasetListRequest {
            context: context(),
            page_cursor: None,
            page_size: 1,
        })
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].name, "验收样本");
    assert_eq!(String::from(result.items[0].asset_count), "2");
    assert_eq!(String::from(result.items[0].unconfirmed_count), "2");
    assert!(result.next_cursor.is_none());

    let dataset_id = id(registered.dataset_id);
    let assets = |cursor, page_size| AssetListRequest {
        context: context(),
        dataset_id: Some(dataset_id.clone()),
        kind: None,
        cursor,
        page_size,
    };
    let first = commands.list_assets(assets(None, 1)).unwrap();
    let next = commands
        .list_assets(assets(first.next_cursor.clone(), 1))
        .unwrap();
    assert!(next.next_cursor.is_none());
    let mut names = vec![
        first.items[0].display_name.as_str(),
        next.items[0].display_name.as_str(),
    ];
    names.sort_unstable();
    assert_eq!(names, ["first.png", "second.png"]);
    assert_eq!(
        first.items[0].annotation_state,
        AnnotationState::Unannotated
    );
    assert_eq!(
        commands.list_assets(assets(None, 0)).unwrap_err().code,
        "INVALID_ARGUMENT"
    );
    let mut stale = assets(None, 1);
    stale.context.workspace_id = id(Uuid::new_v4().to_string());
    let rejected = commands.list_assets(stale).unwrap_err();
    assert_eq!(rejected.code, "STALE_WORKSPACE_SESSION");
    assert_eq!(rejected.request_id, commands.request_id);
    let view = commands
        .annotation(AnnotationGetRequest {
            context: context(),
            asset_id: first.items[0].asset_id.clone(),
        })
        .unwrap();
    assert!(view.formal_coco.is_none());
    assert_eq!(view.annotation_state, AnnotationState::Unannotated);
    let media = MediaDisplayService::new(Arc::clone(&commands.thread), Duration::from_secs(120));
    let locator = commands
        .media_locator(
            &media,
            MediaGetLocatorRequest {
                context: context(),
                asset_id: Some(first.items[0].asset_id.clone()),
                artifact_id: None,
                owner_task_id: None,
                purpose: MediaPurpose::Display,
            },
        )
        .unwrap();
    let request = || {
        Request::builder()
            .uri(&locator.locator)
            .body(Vec::new())
            .unwrap()
    };
    let response = annotation::media_response(&media, request());
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["Content-Type"], "image/png");
    assert_eq!(response.body().as_slice(), image);
    assert_eq!(response.headers()["Cache-Control"], "no-store");
    commands.thread.close(session).unwrap();
    assert_eq!(annotation::media_response(&media, request()).status(), 403);
}
