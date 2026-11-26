use std::{fs, sync::Arc};

use dd_infra::dataset_profile_runtime::DatasetProfileRuntime;
use dd_infra::geometry_runtime::GeometryRuntime;
use dd_infra::workflow_store::{
    WorkflowError, WorkflowGeometryRequest, WorkflowReportRequest, WorkflowStatus,
};
use rusqlite::Connection;
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const COCO: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,3,1],"area":3,"iscrowd":0}]}"#;

#[tokio::test]
async fn aggregate_report_uses_real_check_outcomes_and_preserves_the_human_gate() {
    let f = WorkflowFixture::new("Report Workflow", COCO);
    let started = f
        .store
        .start_profile(f.session, &f.policy, f.start_request(), 5)
        .unwrap();
    let run = started.workflow.run_id;
    let reference = started.workflow.plan_ref.unwrap();
    let mut profile = DatasetProfileRuntime::launch(
        Arc::clone(&f.workspace),
        f.session,
        f.interpreter.clone(),
        f.entrypoint.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        profile.run(started.job.unwrap()).await.unwrap().status,
        "SUCCEEDED"
    );
    profile.shutdown().await.unwrap();
    let admitted = f
        .store
        .start_geometry(
            f.session,
            &f.policy,
            WorkflowGeometryRequest {
                reference: reference.clone(),
                expected_row_revision: started.workflow.row_revision,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                max_pixels: 100,
            },
            6,
        )
        .unwrap();
    let request = WorkflowReportRequest {
        reference,
        expected_row_revision: admitted.workflow.row_revision,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    assert!(matches!(
        f.store
            .build_report(f.session, &f.policy, request.clone(), 7),
        Err(WorkflowError::UpstreamNotReady)
    ));
    let mut geometry = GeometryRuntime::launch(
        Arc::clone(&f.workspace),
        f.session,
        f.interpreter,
        f.entrypoint,
    )
    .await
    .unwrap();
    for job in admitted.jobs {
        assert_eq!(
            geometry.audit_admitted(job).await.unwrap().status,
            "SUCCEEDED"
        );
    }
    geometry.shutdown().await.unwrap();

    let db = Connection::open(f.root.join("workspace.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_healthcheck_report BEFORE UPDATE OF result_manifest_path ON workflow_runs
        BEGIN SELECT RAISE(ABORT, 'synthetic report receipt failure'); END;").unwrap();
    assert!(
        f.store
            .build_report(f.session, &f.policy, request.clone(), 8)
            .is_err()
    );
    let original = f.store.get(f.session, run).unwrap();
    assert_eq!(original.status, WorkflowStatus::Running);
    assert_eq!(original.row_revision, request.expected_row_revision);
    assert!(original.report_task_id.is_none());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM tasks WHERE node_id='build_report'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER fail_healthcheck_report")
        .unwrap();

    let report = f
        .store
        .build_report(f.session, &f.policy, request.clone(), 9)
        .unwrap();
    let snapshot = f.store.get(f.session, run).unwrap();
    assert_eq!(snapshot.status, WorkflowStatus::WaitingReview);
    assert_eq!(snapshot.report_task_id, Some(report.task_id));
    assert_eq!(report.data.report_id, report.reference.artifact_id);
    assert_eq!(report.data.scope.workflow_run_id, run);
    assert_eq!(report.data.scope.dataset_id, f.dataset);
    assert_eq!(report.data.human_ack_status, "PENDING");
    let checks = &report.data.checks;
    let geometry = checks
        .iter()
        .find(|check| check.check_id == "annotation_geometry")
        .unwrap();
    assert_eq!((geometry.processed_assets, geometry.total_assets), (1, 2));
    assert_eq!(geometry.assessment, "FINDINGS");
    assert!(
        geometry
            .findings
            .iter()
            .any(|finding| finding.finding_code == "bbox_out_of_bounds")
    );
    assert!(
        geometry
            .findings
            .iter()
            .any(|finding| finding.finding_code == "annotation_missing")
    );
    assert_eq!(geometry.evidence_refs.len(), 2);
    let visual = checks
        .iter()
        .find(|check| check.check_id == "semantic_review")
        .unwrap();
    assert_eq!(visual.assessment, "NOT_EVALUATED");
    assert_eq!(
        visual.skip_or_failure_reason.as_ref().unwrap().code,
        "MODEL_NOT_CONFIGURED"
    );
    assert!(
        report
            .data
            .unexecuted_checks
            .contains(&"semantic_review".into())
    );
    assert_eq!(report.data.findings_by_severity.get("ERROR"), Some(&1));
    assert_eq!(fs::read(f.source.join("signal.json")).unwrap(), COCO);
    let repeated = f
        .store
        .build_report(f.session, &f.policy, request, 10)
        .unwrap();
    assert_eq!(repeated.reference, report.reference);
    assert_eq!(
        db.query_row("SELECT count(*) FROM review_sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let output: String = db
        .query_row(
            "SELECT result_manifest_path FROM workflow_runs WHERE workflow_run_id=?1",
            [run.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    drop(db);

    f.workspace.close(f.session).unwrap();
    let grant = f
        .workspace
        .grant_selected_directory(f.root.clone())
        .unwrap();
    let reopened = f
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        f.store.get(reopened, run).unwrap().status,
        WorkflowStatus::WaitingReview
    );
    assert_eq!(
        f.store.read_report(reopened, run).unwrap().reference,
        report.reference
    );
    assert!(f.store.read_report(f.session, run).is_err());
    fs::write(f.root.join(output), b"{}").unwrap();
    assert!(f.store.read_report(reopened, run).is_err());
    f.workspace.close(reopened).unwrap();
}
