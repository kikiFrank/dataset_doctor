use std::fs;
use std::sync::Arc;

use dd_infra::staging_commit::{
    AcknowledgedIncompleteCheck, StagingCommitError, StagingCommitRequest, StagingCommitService,
};
use dd_infra::task_store::TaskStoreError;
use dd_infra::workspace_thread::WorkspaceThreadError;
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/staged_sample.rs"]
mod staged_sample;

#[tokio::test]
async fn explicit_admission_requires_current_report_and_preserves_asset_identity() {
    let (fixture, dataset, asset, review, report) = staged_sample::ready().await;
    let session = fixture.session;
    let owner = &fixture.owner;
    let service = StagingCommitService::new(Arc::clone(owner)).unwrap();
    let mut request = StagingCommitRequest {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        target_dataset_id: dataset,
        asset_ids: vec![asset],
        quality_report_ref: report.reference.clone(),
        expected_schema_fingerprint: report.data.scope.label_schema_digest,
        acknowledged_findings: vec![],
        acknowledged_incomplete_checks: vec![],
    };
    assert!(matches!(
        service.commit(session, request.clone(), 310),
        Err(StagingCommitError::AcknowledgementRequired)
    ));
    request
        .acknowledged_incomplete_checks
        .push(AcknowledgedIncompleteCheck {
            check_id: "semantic_review".into(),
            reason: "Operator inspected the sample locally".into(),
        });
    let connection = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let report_path = fixture
        .root
        .join(format!("artifacts/tasks/{}/report.json", report.task_id));
    let report_bytes = fs::read(&report_path).unwrap();
    fs::write(&report_path, b"changed report").unwrap();
    assert!(matches!(
        service.commit(session, request.clone(), 311),
        Err(StagingCommitError::Report(_))
    ));
    fs::write(&report_path, report_bytes).unwrap();
    let active = owner.open_review(session, dataset, asset, 312).unwrap();
    assert!(matches!(
        service.commit(session, request.clone(), 313),
        Err(StagingCommitError::NotReady)
    ));
    owner
        .cancel_review(session, active.review_id, active.row_revision, 314)
        .unwrap();
    let path: String = connection
        .query_row(
            "SELECT path FROM assets WHERE asset_id=?1",
            [asset.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let pixels = fs::read(fixture.root.join(&path)).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_staging_removal BEFORE DELETE ON staging_items
         BEGIN SELECT RAISE(ABORT,'injected Staging removal failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        service.commit(session, request.clone(), 315),
        Err(StagingCommitError::Workspace(WorkspaceThreadError::Task(
            TaskStoreError::Database(_)
        )))
    ));
    assert_empty_membership(&connection);
    connection
        .execute_batch("DROP TRIGGER reject_staging_removal;")
        .unwrap();
    let member = service.commit(session, request.clone(), 316).unwrap();
    assert_eq!(member.accepted_asset_ids, vec![asset]);
    assert_eq!(member.target_dataset_id, dataset);
    assert_eq!(
        member.acknowledged_incomplete_checks,
        request.acknowledged_incomplete_checks
    );
    let (id, final_review, stored_path): (String, String, String) = connection.query_row(
        "SELECT a.asset_id,d.final_review_id,a.path FROM dataset_assets d JOIN assets a USING(asset_id)
         WHERE d.dataset_id=?1", [dataset.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        (id, final_review, stored_path),
        (asset.to_string(), review.to_string(), path.clone())
    );
    assert_eq!(fs::read(fixture.root.join(path)).unwrap(), pixels);
    assert_eq!(fs::read(fixture.source).unwrap(), fixture.original);
    assert_eq!(count(&connection, "SELECT COUNT(*) FROM staging_items"), 0);
    assert_eq!(count(&connection, "SELECT row_revision FROM datasets"), 1);
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0
    );
    drop(connection);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(fixture.root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(service.commit(session, request.clone(), 317).is_err());
    assert_eq!(
        service.commit(reopened, request.clone(), 317).unwrap(),
        member
    );
    request.acknowledged_incomplete_checks[0].reason = "Changed confirmation".into();
    assert!(matches!(
        service.commit(reopened, request, 318),
        Err(StagingCommitError::Workspace(WorkspaceThreadError::Task(
            TaskStoreError::IdempotencyConflict
        )))
    ));
}

fn assert_empty_membership(connection: &Connection) {
    assert_eq!(count(connection, "SELECT COUNT(*) FROM dataset_assets"), 0);
    assert_eq!(count(connection, "SELECT COUNT(*) FROM staging_items"), 1);
    assert_eq!(
        count(
            connection,
            "SELECT COUNT(*) FROM tasks WHERE operation_kind='STAGING_COMMIT'"
        ),
        0
    );
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}
