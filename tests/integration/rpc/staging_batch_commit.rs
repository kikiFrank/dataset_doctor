use std::{fs, sync::Arc};

use dd_domain::video::SeekPolicy;
use dd_infra::frame_runtime::{FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget};
use dd_infra::staging_commit::{
    AcknowledgedIncompleteCheck, StagingCommitError, StagingCommitRequest, StagingCommitService,
};
use dd_infra::staging_report::{StagingBatchReportRequest, StagingReport, StagingReportService};
use dd_infra::video_annotation_store::VideoFrameReference;
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/staged_sample.rs"]
mod staged_sample;

#[tokio::test]
async fn admits_exactly_two_real_reviewed_frames_atomically_under_one_report() {
    let (fixture, dataset, first, first_review, first_report) = staged_sample::ready().await;
    let (second, second_review, second_report) = ready_next(&fixture, dataset, 1).await;
    assert_ne!(first, second);
    let owner = &fixture.owner;
    let session = fixture.session;
    let database = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let reports = StagingReportService::new(Arc::clone(owner)).unwrap();
    let commit = StagingCommitService::new(Arc::clone(owner)).unwrap();
    let build = StagingBatchReportRequest {
        target_dataset_id: dataset,
        source_report_refs: vec![
            first_report.reference.clone(),
            second_report.reference.clone(),
        ],
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let mut forged = build.clone();
    forged.source_report_refs[1].sha256 = "0".repeat(64);
    assert!(reports.build_batch(session, forged, 351).is_err());
    let mut repeated = build.clone();
    repeated.source_report_refs[1] = repeated.source_report_refs[0].clone();
    assert!(reports.build_batch(session, repeated, 352).is_err());
    let batch = reports.build_batch(session, build, 353).unwrap();
    let batch_task = batch.task_id;

    let mut ids = [first, second];
    ids.sort_unstable();
    assert_eq!(batch.asset_ids, ids);
    assert_no_commit(&database);
    let mut request = StagingCommitRequest {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        target_dataset_id: dataset,
        asset_ids: vec![ids[1], ids[0]],
        quality_report_ref: first_report.reference,
        expected_schema_fingerprint: first_report.data.scope.label_schema_digest,
        acknowledged_findings: vec![],
        acknowledged_incomplete_checks: vec![],
    };
    assert!(commit.commit(session, request.clone(), 354).is_err());
    request.quality_report_ref = batch.reference;
    assert!(matches!(
        commit.commit(session, request.clone(), 355),
        Err(StagingCommitError::AcknowledgementRequired)
    ));
    request
        .acknowledged_incomplete_checks
        .push(AcknowledgedIncompleteCheck {
            check_id: "semantic_review".into(),
            reason: "Both frames reviewed locally".into(),
        });
    let second_path = fixture.root.join(format!(
        "artifacts/tasks/{}/report.json",
        second_report.task_id
    ));
    let second_bytes = fs::read(&second_path).unwrap();
    fs::write(&second_path, b"modified single-sample evidence").unwrap();
    assert!(matches!(
        commit.commit(session, request.clone(), 356),
        Err(StagingCommitError::Report(_))
    ));
    assert_no_commit(&database);
    fs::write(second_path, second_bytes).unwrap();
    let pixel_files: Vec<_> = ids
        .iter()
        .map(|asset| {
            let path: String = database
                .query_row(
                    "SELECT path FROM assets WHERE asset_id=?1",
                    [asset.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            let path = fixture.root.join(path);
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();

    database
        .execute_batch(&format!(
            "CREATE TRIGGER reject_second_staged_asset BEFORE DELETE ON staging_items
         WHEN OLD.asset_id='{}'
         BEGIN SELECT RAISE(ABORT,'batch test failure'); END;",
            ids[1]
        ))
        .unwrap();
    assert!(commit.commit(session, request.clone(), 356).is_err());
    assert_no_commit(&database);
    database
        .execute_batch("DROP TRIGGER reject_second_staged_asset;")
        .unwrap();

    let accepted = commit.commit(session, request.clone(), 357).unwrap();
    assert_eq!(accepted.accepted_asset_ids, ids);
    assert_eq!(accepted.target_dataset_id, dataset);
    assert_eq!(
        accepted.acknowledged_incomplete_checks,
        request.acknowledged_incomplete_checks
    );
    assert_eq!(count(&database, "SELECT count(*) FROM staging_items"), 0);
    assert_eq!(count(&database, "SELECT row_revision FROM datasets"), 1);
    assert_eq!(
        count(&database, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
    for (asset, review) in [(first, first_review), (second, second_review)] {
        let stored: String = database
            .query_row(
                "SELECT final_review_id FROM dataset_assets WHERE asset_id=?1 AND dataset_id=?2",
                [asset.to_string(), dataset.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, review.to_string());
    }
    for (path, bytes) in pixel_files {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
    drop(database);

    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(fixture.root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(commit.commit(session, request.clone(), 358).is_err());
    request.asset_ids.reverse();
    assert_eq!(commit.commit(reopened, request, 358).unwrap(), accepted);
    assert_eq!(
        commit.get(reopened, accepted.task_id).unwrap().unwrap(),
        accepted
    );
    let reopened_batch = reports.get_batch(reopened, batch_task).unwrap();
    assert_eq!(reopened_batch.asset_ids, ids);
    assert_eq!(reopened_batch.reference, accepted.quality_report_ref);
    owner.close(reopened).unwrap();
}

fn assert_no_commit(database: &Connection) {
    assert_eq!(count(database, "SELECT count(*) FROM dataset_assets"), 0);
    assert_eq!(count(database, "SELECT count(*) FROM staging_items"), 2);
    assert_eq!(
        count(
            database,
            "SELECT count(*) FROM tasks WHERE operation_kind='STAGING_COMMIT'"
        ),
        0
    );
}

fn count(database: &Connection, sql: &str) -> i64 {
    database.query_row(sql, [], |row| row.get(0)).unwrap()
}

async fn ready_next(
    fixture: &staged_sample::Fixture,
    dataset: Uuid,
    index: u64,
) -> (Uuid, Uuid, StagingReport) {
    let session = fixture.session;
    let frames = FrameService::new(Arc::clone(&fixture.owner)).unwrap();
    let (task, job) = frames
        .submit(
            session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                video_id: fixture.video_id,
                target: FrameTarget::Index(index),
                policy: SeekPolicy::Nearest,
            },
            320,
        )
        .unwrap();
    let mut worker = FrameResolveRuntime::launch(
        Arc::clone(&fixture.owner),
        session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    worker.shutdown().await.unwrap();
    let resolved = frames
        .snapshot(session, task.task_id)
        .unwrap()
        .frame
        .unwrap();
    let frame = VideoFrameReference::from_resolved(task.task_id, &resolved).unwrap();
    staged_sample::stage_ready_sample(fixture, dataset, frame, 330).await
}
