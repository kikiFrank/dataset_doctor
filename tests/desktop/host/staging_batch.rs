use std::{path::PathBuf, sync::Arc};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_ui_contracts::dataset_import::TaskGetRequest;
use dd_ui_contracts::staging::{
    StagingBatchReportBuildRequest, StagingCommitRequest, StagingIncompleteCheckAck,
    StagingListRequest,
};

use super::*;

use super::shared_video_fixture as staged_sample;

#[test]
fn desktop_prepares_and_confirms_an_exact_batch_of_real_staging_frames() {
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
    let (fixture, dataset, [(first, _, first_report), (second, _, second_report)]) =
        tauri::async_runtime::block_on(staged_sample::ready_pair_in(
            &run,
            &repo,
            repo.join("target/dev/worker-venv/Scripts/python.exe"),
        ));
    let session = fixture.session;
    let owner = &fixture.owner;
    let workspace_id = owner.current().unwrap().unwrap().workspace_id;
    let id = |value: String| UuidText::try_from(value).unwrap();
    let context = |session: Uuid| CommandContext {
        request_id: id(Uuid::new_v4().to_string()),
        workspace_id: id(workspace_id.clone()),
        workspace_session_id: id(session.to_string()),
        idempotency_key: Some(id(Uuid::new_v4().to_string())),
    };
    let commands = Commands {
        thread: Arc::clone(owner),
        request_id: id(Uuid::new_v4().to_string()),
    };
    let batch = commands
        .prepare_staging_batch(StagingBatchReportBuildRequest {
            context: context(session),
            target_dataset_id: id(dataset.to_string()),
            source_report_refs: vec![
                commands.wire_artifact(second_report.reference).unwrap(),
                commands.wire_artifact(first_report.reference).unwrap(),
            ],
        })
        .unwrap();
    let mut expected = [first, second];
    expected.sort_unstable();
    let expected_ids = expected.map(|asset| id(asset.to_string()));
    assert_eq!(batch.asset_ids, expected_ids);
    assert_eq!(batch.unexecuted_checks, ["semantic_review"]);
    let list = |session| {
        commands
            .list_staging(StagingListRequest {
                context: context(session),
                target_dataset_id: Some(id(dataset.to_string())),
                cursor: None,
                status: None,
                page_size: 10,
            })
            .unwrap()
    };
    assert_eq!(list(session).items.len(), 2);

    let report_hash = batch.quality_report_ref.sha256.clone();
    let accepted = commands
        .commit_staging(StagingCommitRequest {
            context: context(session),
            target_dataset_id: id(dataset.to_string()),
            asset_ids: expected_ids.iter().rev().cloned().collect(),
            quality_report_ref: batch.quality_report_ref,
            expected_schema_fingerprint: batch.label_schema_digest,
            acknowledged_findings: vec![],
            acknowledged_incomplete_checks: Some(vec![StagingIncompleteCheckAck {
                check_id: "semantic_review".into(),
                reason: "Both frames reviewed on the workstation".into(),
            }]),
        })
        .unwrap();
    assert_eq!(accepted.status, "SUCCEEDED");
    assert!(list(session).items.is_empty());
    let imports = DatasetImportCoordinator::new(Arc::clone(owner)).unwrap();
    let read = |session| {
        commands.import_task(
            &imports,
            TaskGetRequest {
                context: context(session),
                task_id: accepted.task_id.clone(),
            },
        )
    };
    let result = read(session).unwrap().staging_commit.unwrap();
    assert_eq!(result.accepted_asset_ids, expected_ids);
    assert_eq!(result.quality_report_ref.sha256, report_hash);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(fixture.root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(read(session).is_err());
    assert_eq!(
        read(reopened)
            .unwrap()
            .staging_commit
            .unwrap()
            .accepted_asset_ids,
        expected_ids
    );
    assert!(list(reopened).items.is_empty());
    owner.close(reopened).unwrap();
}
