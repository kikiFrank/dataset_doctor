#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;

use std::{fs, sync::Arc};

use dd_infra::report_export::{ReportExportFormat, ReportExportRequest, ReportExportService};
use dd_infra::workflow_store::{LocalHealthcheckRequest, LocalHealthcheckRuntime};
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use workflow_fixture::WorkflowFixture;

const ANNOTATION: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn exports_verified_json_and_markdown_and_reopens_the_durable_receipts() {
    let fixture = WorkflowFixture::new("Report export", ANNOTATION);
    let workspace = &fixture.workspace;
    let session = fixture.session;
    let local = LocalHealthcheckRuntime::new(
        Arc::clone(workspace),
        fixture.interpreter.clone(),
        fixture.entrypoint.clone(),
    )
    .unwrap();
    let accepted = local
        .admit(
            session,
            LocalHealthcheckRequest {
                dataset_id: fixture.dataset,
                sample_limit: 2,
                max_pixels: 100,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let report = local.run(accepted.job.unwrap()).await.unwrap();
    let reference = report.reference;
    let destination = fixture.root.parent().unwrap().join("报告 导出");
    fs::create_dir(&destination).unwrap();
    let exporter = ReportExportService::new(Arc::clone(workspace)).unwrap();
    let grant = workspace
        .grant_export_directory(session, destination.clone())
        .unwrap();

    let request = ReportExportRequest {
        report_ref: reference.clone(),
        format: ReportExportFormat::Json,
        destination_grant_id: grant.grant_id,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let mut forged = request.clone();
    forged.report_ref.sha256 = "0".repeat(64);
    assert!(exporter.submit(session, forged).is_err());
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    let (queued, job) = exporter.submit(session, request.clone()).unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    exporter.run(session, job.unwrap()).unwrap();
    let exported = exporter.get(session, queued.task_id).unwrap();
    assert_eq!(exported.status, "SUCCEEDED");
    let receipt = exported.receipt.unwrap();
    let written = fs::read(destination.join(&receipt.file_name)).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&written)), reference.sha256);
    assert_eq!(receipt.sha256, reference.sha256);
    assert_eq!(receipt.report_ref, reference);
    let document: Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(document["report_id"], reference.artifact_id.to_string());
    assert!(document["checks"].as_array().unwrap().iter().any(|check| {
        check["assessment"] == "FINDINGS" && !check["evidence_refs"].as_array().unwrap().is_empty()
    }));
    assert!(
        document["unexecuted_checks"]
            .as_array()
            .unwrap()
            .contains(&"semantic_review".into())
    );
    let (replayed, replay_job) = exporter.submit(session, request.clone()).unwrap();
    assert_eq!(replayed.task_id, queued.task_id);
    assert!(replay_job.is_none());
    assert!(
        exporter
            .submit(
                session,
                ReportExportRequest {
                    format: ReportExportFormat::Markdown,
                    ..request
                }
            )
            .is_err()
    );

    let grant = workspace
        .grant_export_directory(session, destination.clone())
        .unwrap();
    let (queued_markdown, job) = exporter
        .submit(
            session,
            ReportExportRequest {
                report_ref: reference.clone(),
                format: ReportExportFormat::Markdown,
                destination_grant_id: grant.grant_id,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
            },
        )
        .unwrap();
    let database = Connection::open(fixture.root.join("workspace.db")).unwrap();
    database
        .execute_batch(&format!(
            "CREATE TRIGGER reject_report_receipt BEFORE UPDATE OF status ON tasks
         WHEN OLD.task_id='{}' AND NEW.status='SUCCEEDED'
         BEGIN SELECT RAISE(ABORT, 'report receipt test'); END;",
            queued_markdown.task_id
        ))
        .unwrap();
    assert!(exporter.run(session, job.unwrap()).is_err());
    let pending: String = database
        .query_row(
            "SELECT status FROM file_operations WHERE task_id=?1",
            [queued_markdown.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, "PREPARED");
    let markdown_path = fs::read_dir(&destination)
        .unwrap()
        .map(|item| item.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "md"))
        .unwrap();
    let written = fs::read_to_string(&markdown_path).unwrap();
    assert!(written.contains("MODEL_NOT_CONFIGURED"));
    assert!(written.contains("SKIPPED"));
    assert!(written.contains("annotation_geometry"));
    assert!(written.contains(&reference.artifact_id.to_string()));
    assert!(!written.contains(&fixture.source.display().to_string()));
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 2);
    assert_eq!(
        fs::read(fixture.source.join("signal.json")).unwrap(),
        ANNOTATION
    );
    database
        .execute_batch("DROP TRIGGER reject_report_receipt;")
        .unwrap();
    drop(database);

    workspace.close(session).unwrap();
    let grant = workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = workspace.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(exporter.get(session, queued_markdown.task_id).is_err());
    let historical = exporter
        .get(reopened, queued_markdown.task_id)
        .unwrap()
        .receipt
        .unwrap();
    assert_eq!(
        markdown_path.file_name().unwrap(),
        historical.file_name.as_str()
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(written.as_bytes())),
        historical.sha256
    );
    let database = Connection::open(fixture.root.join("workspace.db")).unwrap();
    let committed: String = database
        .query_row(
            "SELECT status FROM file_operations WHERE task_id=?1",
            [queued_markdown.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(committed, "COMMITTED");
    let integrity: String = database
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    let foreign_key_violations: u32 = database
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_eq!(foreign_key_violations, 0);
    drop(database);
    workspace.close(reopened).unwrap();
}
