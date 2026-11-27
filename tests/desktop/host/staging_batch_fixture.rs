//! Create a closed, reusable two-frame Workspace for the WebView2 acceptance.

use std::fs;
use std::path::PathBuf;

use dd_infra::staging::{StagingQuery, StagingState};
use serde_json::json;

use super::shared_video_fixture as staged_sample;

#[test]
fn desktop_creates_real_staging_batch_ui_fixture() {
    let run = PathBuf::from(std::env::var_os("DD_DESKTOP_COMMANDS_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        run.parent(),
        Some(repo.join("target/desktop-commands/runs").as_path())
    );
    let (fixture, dataset, [(first, _, _), (second, _, _)]) =
        tauri::async_runtime::block_on(staged_sample::ready_pair_in(
            &run,
            &repo,
            repo.join("target/dev/worker-venv/Scripts/python.exe"),
        ));
    let page = fixture
        .owner
        .list_staging(
            fixture.session,
            StagingQuery {
                target_dataset_id: Some(dataset),
                after: None,
                status: None,
                page_size: 10,
            },
        )
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(
        page.items
            .iter()
            .all(|item| item.state == StagingState::ReadyForAcceptance)
    );
    fixture.owner.close(fixture.session).unwrap();
    let mut assets = [first, second];
    assets.sort_unstable();
    let plan = run.join("staging-batch-ui.json");
    fs::write(
        &plan,
        serde_json::to_vec_pretty(&json!({
            "workspace":fixture.root, "source_video":fixture.source,
            "dataset_id":dataset,"asset_ids":assets,
        }))
        .unwrap(),
    )
    .unwrap();
    println!("Real Staging fixture: {}", plan.display());
}
