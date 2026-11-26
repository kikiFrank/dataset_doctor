use std::{fs, path::PathBuf, process::Command, time::Duration};

use dd_infra::worker_process::WorkerProcessError;
use dd_protocol::v1::{ImageQualityInput, ImageQualityPolicy, StartImageQualityInspectionRequest};
use serde_json::{Value, json};
use tonic::Code;
use uuid::Uuid;

#[path = "support/worker_task.rs"]
mod worker_task;
use worker_task::{WorkerTask, digest, file_locator, run_root};

const CAPABILITY: &str = "image_quality.v1";
const DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartImageQualityInspection\0";

fn prepare(
    case: &str,
    path: PathBuf,
    source: &[u8],
) -> (WorkerTask, StartImageQualityInspectionRequest) {
    let task = WorkerTask::new(
        case,
        "IMAGE_QUALITY",
        CAPABILITY,
        Uuid::new_v4(),
        &digest(source),
        json!({
            "max_pixels": 4, "quality_policy": { "dark_luma_max":20, "bright_luma_min":235,
                "exposure_fraction":0.9, "min_gradient_energy":100.0 },
        }),
    );
    let input = ImageQualityInput {
        image: Some(file_locator(&path, source)),
        input_manifest: Some(task.manifest_locator()),
        output_directory: task.output.to_str().unwrap().into(),
        max_pixels: 4,
        policy: Some(ImageQualityPolicy {
            dark_luma_max: 20,
            bright_luma_min: 235,
            exposure_fraction: 0.9,
            min_gradient_energy: 100.0,
        }),
    };
    let (context, manifest) = task.dispatch(DOMAIN, &input);
    (
        task,
        StartImageQualityInspectionRequest {
            context: Some(context),
            input_manifest_ref: Some(manifest),
            parameters: Some(input),
        },
    )
}

#[tokio::test]
async fn real_quality_worker_binds_frozen_thresholds_and_seals_darkness_and_blur() {
    let path = run_root().join("opaque-dark-quality.png");
    let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    assert!(Command::new(python).args(["-I", "-B", "-c",
        "import sys; from PIL import Image; Image.new('RGB',(2,2),(20,20,20)).save(sys.argv[1])"])
        .arg(&path).status().unwrap().success());
    let bytes = fs::read(&path).unwrap();
    let (task, request) = prepare("Real Image Quality", path.clone(), &bytes);
    let mut live = task.launch().await;
    assert!(
        live.worker
            .health()
            .await
            .unwrap()
            .loaded_capability_ids
            .contains(&CAPABILITY.into())
    );
    let mut altered = request.clone();
    altered
        .parameters
        .as_mut()
        .unwrap()
        .policy
        .as_mut()
        .unwrap()
        .dark_luma_max = 1;
    assert!(matches!(live.worker.start_image_quality(altered).await,
        Err(WorkerProcessError::Rpc(status)) if status.code() == Code::PermissionDenied));
    assert!(fs::read_dir(&task.output).unwrap().next().is_none());
    let first = live
        .worker
        .start_image_quality(request.clone())
        .await
        .unwrap();
    assert_eq!(
        live.worker.start_image_quality(request).await.unwrap(),
        first
    );
    assert_eq!(
        live.wait(&first, Duration::from_secs(10)).await.status,
        "SUCCEEDED"
    );
    assert_eq!(task.outcome().status, "SUCCEEDED");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let result: Value =
        serde_json::from_slice(&fs::read(task.output.join("image-quality.json")).unwrap()).unwrap();
    assert_eq!(result["input_fingerprint"], "12".repeat(32));
    assert_eq!(result["image"]["width"], 2);
    assert_eq!(result["measurements"]["visible_pixels"], 4);
    assert_eq!(result["measurements"]["dark_fraction"].as_f64(), Some(1.0));
    assert_eq!(
        result["measurements"]["gradient_energy"].as_f64(),
        Some(0.0)
    );
    assert_eq!(result["policy"]["dark_luma_max"], 20);
    assert_eq!(result["checks"][0]["assessment"], "PASS");
    assert_eq!(result["checks"][1]["check_id"], "image.underexposed");
    assert_eq!(result["checks"][1]["assessment"], "FINDINGS");
    assert_eq!(result["checks"][2]["assessment"], "PASS");
    assert_eq!(result["checks"][3]["check_id"], "image.blur");
    assert_eq!(result["checks"][3]["assessment"], "FINDINGS");
    live.shutdown().await;
}

#[tokio::test]
async fn undecodable_inputs_record_the_file_issue_without_evaluating_pixels() {
    let path = run_root().join("malformed-quality.png");
    let bytes = b"not an image";
    fs::write(&path, bytes).unwrap();
    let (task, request) = prepare("Undecodable Image Quality", path, bytes);
    let mut live = task.launch().await;
    let accepted = live.worker.start_image_quality(request).await.unwrap();
    assert_eq!(
        live.wait(&accepted, Duration::from_secs(10)).await.status,
        "SUCCEEDED"
    );
    let result: Value =
        serde_json::from_slice(&fs::read(task.output.join("image-quality.json")).unwrap()).unwrap();
    assert!(result["measurements"].is_null());
    assert_eq!(result["checks"][0]["execution_status"], "FAILED");
    assert_eq!(result["checks"][0]["reason_code"], "decode_failed");
    assert!(
        result["checks"].as_array().unwrap()[1..]
            .iter()
            .all(|check| check["execution_status"] == "SKIPPED"
                && check["assessment"] == "NOT_EVALUATED")
    );
    assert_eq!(task.outcome().status, "SUCCEEDED");
    live.shutdown().await;
}

#[tokio::test]
async fn changed_source_does_not_publish_a_decode_failure_report() {
    let path = run_root().join("changed-quality.png");
    let bytes = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
    fs::write(&path, bytes).unwrap();
    let (task, request) = prepare("Changed Image Quality", path.clone(), bytes);
    fs::write(path, b"different bytes after authorization").unwrap();
    let mut live = task.launch().await;
    let accepted = live.worker.start_image_quality(request).await.unwrap();
    let result = live.wait(&accepted, Duration::from_secs(10)).await;
    assert_eq!(result.status, "FAILED");
    assert_eq!(result.error_code, "input_changed");
    assert_eq!(task.outcome().error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(fs::read_dir(&task.output).unwrap().next().is_none());
    live.shutdown().await;
}
