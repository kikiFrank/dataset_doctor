use std::fs;
use std::time::Duration;

use dd_infra::worker_process::WorkerProcessError;
use dd_protocol::v1::{DatasetProfileAsset, DatasetProfileInput, DatasetProfileRequest};
use serde_json::{Value, json};
use tonic::Code;
use uuid::Uuid;

#[path = "support/worker_task.rs"]
mod worker_task;
use worker_task::{WorkerTask, file_locator, run_root};

const CAPABILITY: &str = "dataset_profile.v1";
const DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartProfileDataset\0";
const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const BAD_IMAGE: &[u8] = b"synthetic undecodable image";
const COCO: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[{"id":1,"image_id":1,"category_id":7,"bbox":[0,0,1,1],"area":1,"iscrowd":0}]}"#;

fn profile_fixture(case: &str) -> (WorkerTask, DatasetProfileInput) {
    let directory = run_root().join(format!("{case} Sources"));
    fs::create_dir(&directory).unwrap();
    let mut inputs = Vec::new();
    let mut entries = Vec::new();
    for (name, bytes, labels) in [
        ("valid", PNG, Some(COCO)),
        ("undecodable", BAD_IMAGE, None),
        ("offline-media", PNG, None),
        ("offline-labels", PNG, Some(COCO)),
    ] {
        let id = Uuid::new_v4();
        let image = directory.join(format!("{name}.png"));
        fs::write(&image, bytes).unwrap();
        let image = file_locator(&image, bytes);
        let annotation = labels.map(|bytes| {
            let path = directory.join(format!("{name}.json"));
            fs::write(&path, bytes).unwrap();
            file_locator(&path, bytes)
        });
        entries.push(json!({
            "asset_id": id, "media_digest": image.sha256,
            "annotation_digest": annotation.as_ref().map(|item| &item.sha256),
            "source_metadata_digest": null,
        }));
        inputs.push(DatasetProfileAsset {
            asset_id: id.to_string(),
            image: Some(image),
            annotation,
        });
    }
    let task = WorkerTask::with_assets(
        case,
        "DATASET_PROFILE",
        CAPABILITY,
        entries,
        json!({"max_pixels": 2}),
    );
    let input = DatasetProfileInput {
        assets: inputs,
        input_manifest: Some(task.manifest_locator()),
        output_directory: task.output.to_str().unwrap().into(),
        max_pixels: 2,
    };
    (task, input)
}

#[tokio::test]
async fn profiles_the_frozen_assets_through_the_real_worker_and_seals_partial_file_errors() {
    let (task, input) = profile_fixture("Dataset Profile");
    let bad_asset = input.assets[1].asset_id.clone();
    let offline = &input.assets[2];
    let offline_labels = &input.assets[3];
    fs::remove_file(&offline.image.as_ref().unwrap().path).unwrap();
    fs::remove_file(&offline_labels.annotation.as_ref().unwrap().path).unwrap();
    let offline_assets = [offline.asset_id.clone(), offline_labels.asset_id.clone()];
    let original = input.assets[0].image.as_ref().unwrap().path.clone();
    let (context, manifest) = task.dispatch(DOMAIN, &input);
    let mut live = task.launch().await;
    let request = DatasetProfileRequest {
        context: Some(context),
        input_manifest_ref: Some(manifest),
        parameters: Some(input),
    };
    let mut altered = request.clone();
    altered.parameters.as_mut().unwrap().assets.pop();
    assert!(matches!(live.worker.start_dataset_profile(altered).await,
        Err(WorkerProcessError::Rpc(status)) if status.code() == Code::PermissionDenied));
    assert!(fs::read_dir(&task.output).unwrap().next().is_none());
    assert!(
        live.worker
            .health()
            .await
            .unwrap()
            .loaded_capability_ids
            .contains(&CAPABILITY.into())
    );
    let execution = live
        .worker
        .start_dataset_profile(request.clone())
        .await
        .unwrap();
    assert_eq!(
        live.worker.start_dataset_profile(request).await.unwrap(),
        execution
    );
    let completed = live.wait(&execution, Duration::from_secs(10)).await;
    assert_eq!(completed.status, "SUCCEEDED", "{completed:?}");
    assert_eq!(task.outcome().status, "SUCCEEDED");
    assert_eq!(fs::read(original).unwrap(), PNG);
    let profile: Value =
        serde_json::from_slice(&fs::read(task.output.join("dataset-profile.json")).unwrap())
            .unwrap();
    assert_eq!(profile["input_fingerprint"], "12".repeat(32));
    assert_eq!(profile["asset_count"], 4);
    assert_eq!(profile["annotation_count"], 1);
    assert_eq!(
        profile["resolution_histogram"],
        json!([{"width":2,"height":1,"count":2}])
    );
    assert_eq!(profile["class_counts"], json!({"green-light": 1}));
    assert_eq!(profile["empty_annotation_count"], 0);
    assert_eq!(profile["unannotated_count"], 2);
    let errors_path = task.output.join("dataset-profile-file-errors.json");
    let errors: Value = serde_json::from_slice(&fs::read(&errors_path).unwrap()).unwrap();
    assert_eq!(
        errors,
        json!([
            {"asset_id":bad_asset,"source":"image","code":"decode_failed"},
            {"asset_id":offline_assets[0],"source":"image","code":"offline"},
            {"asset_id":offline_assets[1],"source":"annotation","code":"offline"},
        ])
    );
    let result: Value =
        serde_json::from_slice(&fs::read(task.output.join("result-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(result["entries"].as_array().unwrap().len(), 2);
    assert_eq!(
        profile["file_errors_ref"]["artifact_id"],
        result["entries"][1]["artifact_id"]
    );
    assert_eq!(
        profile["file_errors_ref"]["sha256"],
        result["entries"][1]["sha256"]
    );
    live.shutdown().await;
}

#[tokio::test]
async fn source_digest_changes_end_profiling_without_publishing_results() {
    let (task, input) = profile_fixture("Changed Dataset Profile");
    let (context, manifest) = task.dispatch(DOMAIN, &input);
    fs::write(&input.assets[0].image.as_ref().unwrap().path, BAD_IMAGE).unwrap();
    let mut live = task.launch().await;
    let execution = live
        .worker
        .start_dataset_profile(DatasetProfileRequest {
            context: Some(context),
            input_manifest_ref: Some(manifest),
            parameters: Some(input),
        })
        .await
        .unwrap();
    let completed = live.wait(&execution, Duration::from_secs(10)).await;
    assert_eq!(completed.status, "FAILED");
    assert_eq!(completed.error_code, "input_changed");
    assert_eq!(task.outcome().error_code.as_deref(), Some("INPUT_CHANGED"));
    assert!(fs::read_dir(&task.output).unwrap().next().is_none());
    live.shutdown().await;
}
