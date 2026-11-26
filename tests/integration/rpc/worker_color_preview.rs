use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use dd_infra::worker_process::WorkerProcessError;
use dd_protocol::v1::{
    ColorMethod, ColorOperation, ColorPoint, ColorPreviewInput, ColorPreviewRequest,
    ColorRectangle, color_preview_input,
};
use serde_json::{Value, json};
use tonic::Code;
use uuid::Uuid;

#[path = "support/worker_task.rs"]
mod worker_task;
use worker_task::{WorkerTask, digest, file_locator, run_root};

const CAPABILITY: &str = "color_preview.v1";
const DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartColorPreview\0";
const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[tokio::test]
async fn real_color_preview_requires_its_grant_and_seals_pixels_without_modifying_the_source() {
    let image = run_root().join("original.png");
    fs::write(&image, PNG).unwrap();
    let task = WorkerTask::new(
        "Color Preview",
        "COLOR_PREVIEW",
        CAPABILITY,
        Uuid::new_v4(),
        &digest(PNG),
        json!({"max_pixels": 2}),
    );
    let image_locator = file_locator(&image, PNG);
    let input = ColorPreviewInput {
        image: Some(image_locator.clone()),
        input_manifest: Some(task.manifest_locator()),
        output_directory: task.output.to_str().unwrap().into(),
        max_pixels: 2,
        region: Some(color_preview_input::Region::Rectangle(ColorRectangle {
            origin: Some(ColorPoint { x: 0.0, y: 0.0 }),
            width: 1.0,
            height: 1.0,
        })),
        operation: Some(ColorOperation {
            method: ColorMethod::SolidFill.into(),
            target_rgb: vec![17, 30, 90],
            ..Default::default()
        }),
    };
    let (context, manifest_ref) = task.dispatch(DOMAIN, &input);
    let mut live = task.launch().await;
    let request = ColorPreviewRequest {
        context: Some(context),
        input_manifest_ref: Some(manifest_ref),
        parameters: Some(input),
    };
    let mut altered = request.clone();
    altered
        .parameters
        .as_mut()
        .unwrap()
        .operation
        .as_mut()
        .unwrap()
        .target_rgb[0] = 100;
    let rejected = live.worker.start_color_preview(altered).await.unwrap_err();
    assert!(
        matches!(&rejected, WorkerProcessError::Rpc(status) if status.code() == Code::PermissionDenied),
        "{rejected:?}",
    );
    assert!(fs::read_dir(&task.output).unwrap().next().is_none());
    assert!(
        live.worker
            .health()
            .await
            .unwrap()
            .loaded_capability_ids
            .contains(&CAPABILITY.into())
    );

    let handle = live
        .worker
        .start_color_preview(request.clone())
        .await
        .unwrap();
    assert_eq!(
        live.worker.start_color_preview(request).await.unwrap(),
        handle
    );
    let completed = live.wait(&handle, Duration::from_secs(10)).await;
    assert_eq!(completed.status, "SUCCEEDED", "{completed:?}");
    assert_eq!(task.outcome().status, "SUCCEEDED");
    assert_eq!(fs::read(&image).unwrap(), PNG);
    let candidate = task.output.join("candidate.png");
    let mask = task.output.join("mask.png");
    let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let status = Command::new(&python).args(["-I", "-B", "-c",
        "import sys; from PIL import Image; original, candidate, mask = map(Image.open, sys.argv[1:]); assert candidate.getpixel((0,0)) == (17,30,90); assert candidate.getpixel((1,0)) == original.getpixel((1,0)); assert mask.get_flattened_data() == (255, 0)"])
        .args([&image, &candidate, &mask]).status().unwrap();
    assert!(status.success());
    let details: Value =
        serde_json::from_slice(&fs::read(task.output.join("color-preview.json")).unwrap()).unwrap();
    assert_eq!(details["base_image_digest"], image_locator.sha256);
    assert_eq!(
        details["region"],
        json!({"kind": "RECT", "bbox_xywh": [0.0, 0.0, 1.0, 1.0]})
    );
    assert_eq!(details["operation"]["target_rgb"], json!([17, 30, 90]));
    assert_eq!(details["operation"]["strength"], 1);
    assert_eq!(details["changed_pixels"], 1);
    live.shutdown().await;
}
