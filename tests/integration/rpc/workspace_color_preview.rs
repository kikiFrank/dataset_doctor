use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dd_infra::color_preview::{
    ColorGate, ColorOptions, ColorPreviewRuntime, ColorPreviewService, ColorPreviewSubmission,
    ColorRegion, RepairColorMethod,
};
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/review.rs"]
mod review;
use review::ReviewFixture;

#[tokio::test]
async fn workspace_preview_freezes_color_intent_and_verifies_the_exact_worker_output() {
    let fixture = ReviewFixture::open().await;
    let owner = &fixture.owner;
    let session = fixture.session;
    let image = fixture.formal.with_extension("png");
    let service = ColorPreviewService::new(Arc::clone(owner)).unwrap();
    let request = ColorPreviewSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        dataset_id: fixture.opened.dataset_id,
        asset_id: fixture.opened.asset_id,
        base_image_digest: fixture.opened.base_image_digest.clone(),
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
    };
    let mut stale = request.clone();
    stale.base_image_digest = "0".repeat(64);
    assert!(service.submit(session, stale, 20).is_err());
    let mut invalid = request.clone();
    invalid.options.strength = f64::NAN;
    assert!(service.submit(session, invalid, 20).is_err());
    let mut outside = request.clone();
    outside.asset_id = Uuid::new_v4();
    assert!(service.submit(session, outside, 20).is_err());
    let (task, job) = service.submit(session, request.clone(), 20).unwrap();
    assert_eq!(task.status, "QUEUED");
    let same = service.submit(session, request.clone(), 21).unwrap();
    assert_eq!(same.0.task_id, task.task_id);
    assert!(same.1.is_none());
    let mut conflict = request.clone();
    conflict.options.target_rgb[0] = 100;
    assert!(service.submit(session, conflict, 21).is_err());
    assert_eq!(fs::read(&image).unwrap(), review::IMAGE);
    assert_eq!(fs::read(&fixture.formal).unwrap(), review::ORIGINAL);

    let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entry = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/python-worker/src/dd_worker/entrypoint.py")
        .canonicalize()
        .unwrap();
    let mut worker = ColorPreviewRuntime::launch(Arc::clone(owner), session, python.clone(), entry)
        .await
        .unwrap();
    let result = worker.run(job.unwrap()).await.unwrap();
    assert_eq!(result.status, "SUCCEEDED", "{:?}", result.error_code);
    let mut zero = request.clone();
    zero.idempotency_key = Uuid::new_v4();
    zero.request_id = Uuid::new_v4();
    zero.region = ColorRegion::Polygon {
        points_xy: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    };
    zero.options.method = RepairColorMethod::HueRemap;
    zero.options.strength = 0.0;
    zero.options.source_color_gate = Some(ColorGate {
        hue_min_deg: 350.0,
        hue_max_deg: 10.0,
        wraps_zero: true,
    });
    let (zero_task, job) = service.submit(session, zero, 22).unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    let unchanged = service
        .snapshot(session, zero_task.task_id)
        .unwrap()
        .preview
        .unwrap();
    assert_eq!(unchanged.changed_pixels, 0);
    assert_eq!(unchanged.candidate_ref.sha256, request.base_image_digest);
    let submit_again = |now| {
        let mut new = request.clone();
        new.idempotency_key = Uuid::new_v4();
        new.request_id = Uuid::new_v4();
        service.submit(session, new, now).unwrap()
    };
    let (changed_task, job) = submit_again(23);
    fs::write(&image, b"source changed after approval").unwrap();
    let changed = worker.run(job.unwrap()).await.unwrap();
    assert_eq!(changed.status, "FAILED");
    assert_eq!(changed.error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(
        service
            .snapshot(session, changed_task.task_id)
            .unwrap()
            .preview
            .is_none()
    );
    fs::write(&image, review::IMAGE).unwrap();
    let (pending, _) = submit_again(24);
    worker.shutdown().await.unwrap();
    let result = service.snapshot(session, task.task_id).unwrap();
    let preview = result.preview.unwrap();
    assert_eq!(preview.asset_id, request.asset_id);
    assert_eq!(preview.base_image_digest, request.base_image_digest);
    assert_eq!(preview.changed_pixels, 1);
    assert_eq!(preview.candidate_ref.kind, "REPAIR_CANDIDATE");
    let checked = Command::new(python).args(["-I", "-B", "-c",
        "import sys; from PIL import Image; original, candidate, mask = map(Image.open, sys.argv[1:]); assert candidate.getpixel((0,0)) == (17,30,90); assert candidate.getpixel((1,0)) == original.getpixel((1,0)); assert mask.get_flattened_data() == (255,0)"])
        .args([&image, &fixture.root.join(&preview.candidate_path), &fixture.root.join(&preview.mask_path)])
        .status().unwrap();
    assert!(checked.success());
    assert_eq!(fs::read(&image).unwrap(), review::IMAGE);
    assert_eq!(fs::read(&fixture.formal).unwrap(), review::ORIGINAL);
    owner.close(session).unwrap();
    let folder = owner
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = owner.open(folder.grant_id).unwrap().workspace_session_id;
    assert!(service.snapshot(session, task.task_id).is_err());
    let interrupted = service.snapshot(reopened, pending.task_id).unwrap();
    assert_eq!(interrupted.status, "INTERRUPTED");
    assert_eq!(interrupted.error_code.as_deref(), Some("SESSION_ENDED"));
    let restored = service
        .snapshot(reopened, task.task_id)
        .unwrap()
        .preview
        .unwrap();
    assert_eq!(restored.candidate_ref, preview.candidate_ref);
    fs::write(
        fixture.root.join(restored.candidate_path),
        b"changed candidate artifact",
    )
    .unwrap();
    assert!(service.snapshot(reopened, task.task_id).is_err());
}
