use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dd_infra::annotation_view::AnnotationReader;
use dd_infra::color_apply::{ColorApplyService, ColorApplyState, ColorApplySubmission};
use dd_infra::color_preview::{
    ColorOptions, ColorPreviewRuntime, ColorPreviewService, ColorPreviewSubmission, ColorRegion,
    RepairColorMethod,
};
use dd_infra::dataset_query::AssetAnnotationState;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime, read_geometry_report};
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/review.rs"]
mod review;
use review::ReviewFixture;

#[tokio::test]
async fn explicitly_approved_color_apply_recovers_exact_bytes_and_invalidates_prior_review() {
    let fixture = ReviewFixture::open().await;
    let owner = &fixture.owner;
    let session = fixture.session;
    let image = fixture.formal.with_extension("png");
    let asset = fixture.opened.asset_id;
    let dataset = fixture.opened.dataset_id;
    let mut document: serde_json::Value = serde_json::from_slice(review::ORIGINAL).unwrap();
    let category = owner.dataset_detail(session, dataset).unwrap().categories[0].id;
    document["categories"][0]["id"] = json!(category);
    document["annotations"][0]["category_id"] = json!(category);
    document["annotations"][0]["bbox"] = json!([0, 0, 1, 1]);
    document["annotations"][0]["area"] = json!(1);
    let saved = ReviewDraftService::new(Arc::clone(owner))
        .unwrap()
        .save(
            session,
            ReviewDraftRequest {
                review_id: fixture.opened.review_id,
                source_task_id: fixture.task,
                expected_review_revision: fixture.opened.row_revision,
                expected_draft_revision: None,
                coco_document: serde_json::to_vec(&document).unwrap(),
            },
            20,
        )
        .unwrap();
    let finalizer = ReviewFinalService::new(Arc::clone(owner)).unwrap();
    let grant = finalizer
        .authorize_write(
            session,
            fixture.opened.review_id,
            fixture.formal.parent().unwrap().to_owned(),
        )
        .unwrap();
    let final_op = finalizer
        .prepare_with_write_grant(
            session,
            ReviewFinalRequest {
                review_id: saved.review_id,
                expected_review_revision: saved.review_revision,
                base_image_digest: fixture.opened.base_image_digest.clone(),
                base_annotation_digest: fixture.opened.base_annotation_digest.clone(),
                draft_digest: saved.draft_digest,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                confirmed_negative: false,
            },
            grant.grant_id,
            21,
        )
        .unwrap();
    assert_eq!(
        finalizer
            .resume(session, final_op.operation_id, 22)
            .unwrap()
            .state,
        FinalOperationState::Committed
    );
    let reader = AnnotationReader::new(Arc::clone(owner)).unwrap();
    let before = reader.read(session, asset).unwrap();
    assert_eq!(
        before.annotation_state,
        AssetAnnotationState::ConfirmedPositive
    );
    let before_annotation = fs::read(&fixture.formal).unwrap();
    let preview_service = ColorPreviewService::new(Arc::clone(owner)).unwrap();
    let (task, job) = preview_service
        .submit(
            session,
            ColorPreviewSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                dataset_id: dataset,
                asset_id: asset,
                base_image_digest: before.image_digest.clone(),
                region: ColorRegion::Rectangle {
                    bbox_xywh: [0.0, 0.0, 1.0, 1.0],
                },
                options: ColorOptions {
                    method: RepairColorMethod::SolidFill,
                    target_rgb: [17, 30, 90],
                    strength: 1.0,
                    feather_px: 0.0,
                    source_color_gate: None,
                },
            },
            23,
        )
        .unwrap();
    let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entry = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/python-worker/src/dd_worker/entrypoint.py")
        .canonicalize()
        .unwrap();
    let mut worker =
        ColorPreviewRuntime::launch(Arc::clone(owner), session, python.clone(), entry.clone())
            .await
            .unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    worker.shutdown().await.unwrap();
    let candidate = preview_service
        .snapshot(session, task.task_id)
        .unwrap()
        .preview
        .unwrap();
    let exact = fs::read(fixture.root.join(&candidate.candidate_path)).unwrap();
    let service = ColorApplyService::new(Arc::clone(owner)).unwrap();
    let request = ColorApplySubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        dataset_id: dataset,
        asset_id: asset,
        candidate_ref: candidate.candidate_ref.clone(),
        base_image_digest: before.image_digest.clone(),
        expected_asset_revision: candidate.asset_revision,
    };
    let mut forged = request.clone();
    forged.candidate_ref.sha256 = "0".repeat(64);
    assert!(service.prepare(session, forged, 24).is_err());
    let mut stale = request.clone();
    stale.expected_asset_revision += 1;
    assert!(service.prepare(session, stale, 24).is_err());
    let mut another = request.clone();
    another.idempotency_key = Uuid::new_v4();
    another.request_id = Uuid::new_v4();
    let overtaken = service.prepare(session, another.clone(), 24).unwrap();
    let replacement = b"third-party bytes after approval";
    fs::write(&image, replacement).unwrap();
    assert_eq!(
        service
            .resume(session, overtaken.operation_id, 25)
            .unwrap()
            .state,
        ColorApplyState::Conflict
    );
    assert_eq!(fs::read(&image).unwrap(), replacement);
    assert_eq!(
        service.prepare(session, another.clone(), 25).unwrap().state,
        ColorApplyState::Conflict
    );
    fs::write(&image, review::IMAGE).unwrap();
    let prepared = service.prepare(session, request.clone(), 24).unwrap();
    assert_eq!(prepared.state, ColorApplyState::Prepared);
    assert_eq!(
        service.snapshot(session, prepared.task_id).unwrap().state,
        ColorApplyState::Prepared
    );
    assert_eq!(fs::read(&image).unwrap(), review::IMAGE);
    another.idempotency_key = Uuid::new_v4();
    assert!(service.prepare(session, another, 24).is_err());
    assert_eq!(
        service
            .prepare(session, request.clone(), 24)
            .unwrap()
            .operation_id,
        prepared.operation_id
    );
    let payload = fixture.root.join(format!(
        "artifacts/file-operations/{}/image.payload",
        prepared.operation_id
    ));
    fs::write(&payload, b"changed reviewed color candidate").unwrap();
    assert!(service.resume(session, prepared.operation_id, 25).is_err());
    assert_eq!(fs::read(&image).unwrap(), review::IMAGE);
    fs::write(payload, &exact).unwrap();
    let db = Connection::open(fixture.root.join("workspace.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER abort_color_apply BEFORE UPDATE OF media_digest ON assets
        BEGIN SELECT RAISE(ABORT,'synthetic recovered color apply'); END;",
    )
    .unwrap();
    assert!(service.resume(session, prepared.operation_id, 25).is_err());
    assert_eq!(fs::read(&image).unwrap(), exact);
    let old_digest: String = db
        .query_row(
            "SELECT media_digest FROM assets WHERE asset_id=?1",
            [asset.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_digest, before.image_digest);
    db.execute_batch("DROP TRIGGER abort_color_apply").unwrap();
    drop(db);
    owner.close(session).unwrap();
    let grant = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let committed = service.resume(reopened, prepared.operation_id, 26).unwrap();
    assert_eq!(committed.state, ColorApplyState::Committed);
    assert_eq!(
        service.snapshot(reopened, prepared.task_id).unwrap().state,
        ColorApplyState::Committed
    );
    assert_eq!(
        service
            .prepare(reopened, request.clone(), 26)
            .unwrap()
            .operation_id,
        prepared.operation_id
    );
    let mut replay = request;
    replay.request_id = Uuid::new_v4();
    replay.idempotency_key = Uuid::new_v4();
    assert!(service.prepare(reopened, replay, 26).is_err());
    let after = reader.read(reopened, asset).unwrap();
    assert_eq!(after.annotation_state, AssetAnnotationState::Stale);
    assert_eq!(
        owner.get_review(reopened, saved.review_id).unwrap().status,
        dd_infra::review_store::ReviewStatus::Stale
    );
    assert_eq!(after.image_digest, candidate.candidate_ref.sha256);
    assert_eq!(after.annotation_digest, before.annotation_digest);
    assert_eq!(after.formal_coco, before.formal_coco);
    assert_eq!(fs::read(&fixture.formal).unwrap(), before_annotation);
    assert_eq!(fs::read(&image).unwrap(), exact);
    let mut geometry = GeometryRuntime::launch(Arc::clone(owner), reopened, python, entry)
        .await
        .unwrap();
    let checked = geometry
        .audit(GeometryAuditRequest {
            dataset_id: dataset,
            asset_id: asset,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            max_pixels: 100,
        })
        .await
        .unwrap();
    assert_eq!(checked.status, "SUCCEEDED");
    geometry.shutdown().await.unwrap();
    let report = read_geometry_report(owner, reopened, checked.task_id).unwrap();
    assert_eq!(report.image.sha256, after.image_digest);
    assert_eq!(
        reader.read(reopened, asset).unwrap().annotation_state,
        AssetAnnotationState::Stale
    );
}
