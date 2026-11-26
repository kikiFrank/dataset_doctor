use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::image_quality_runtime::{
    ImageQualityRequest, ImageQualityRuntime, ImageQualityThresholds, read_image_quality,
};
use dd_infra::workspace_thread::WorkspaceThread;
use uuid::Uuid;

#[tokio::test]
async fn registered_sample_quality_is_bound_to_its_scope_and_verified_after_reopen() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        run.parent(),
        Some(repo.join("target/contract-check/worker-rpc").as_path())
    );
    let root = run.join("Image Quality Workspace");
    let source = run.join("Image Quality Sources");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    let image = source.join("dark.png");
    let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    assert!(Command::new(&interpreter).args(["-I","-B","-c",
        "import sys; from PIL import Image; Image.new('RGB',(2,2),(20,20,20)).save(sys.argv[1])"])
        .arg(&image).status().unwrap().success());
    let original = fs::read(&image).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Quality".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner.grant_import_directory(session, source).unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Quality samples".into())
        .unwrap();
    let dataset = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap()
        .dataset_id
        .parse()
        .unwrap();
    let asset = owner.list_assets(session, dataset, None, 1).unwrap().items[0].asset_id;
    let request = ImageQualityRequest {
        dataset_id: dataset,
        asset_id: asset,
        max_pixels: 4,
        thresholds: ImageQualityThresholds {
            dark_luma_max: 20,
            bright_luma_min: 235,
            exposure_fraction: 0.9,
            min_gradient_energy: 100.0,
        },
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
    };
    let mut invalid = request.clone();
    invalid.thresholds.exposure_fraction = 0.0;
    assert!(ImageQualityRuntime::admit(&owner, session, invalid).is_err());
    assert!(
        ImageQualityRuntime::admit(
            &owner,
            session,
            ImageQualityRequest {
                asset_id: Uuid::new_v4(),
                ..request.clone()
            }
        )
        .is_err()
    );
    let job = ImageQualityRuntime::admit(&owner, session, request.clone()).unwrap();
    let task = job.task().task_id;
    assert_eq!(job.task().status, "QUEUED");
    assert!(read_image_quality(&owner, session, dataset, asset, task).is_err());
    assert_eq!(
        ImageQualityRuntime::admit(&owner, session, request.clone())
            .unwrap()
            .task()
            .task_id,
        task
    );
    let mut conflicting = request;
    conflicting.thresholds.min_gradient_energy = 101.0;
    assert!(ImageQualityRuntime::admit(&owner, session, conflicting).is_err());
    let entry = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
    let mut worker = ImageQualityRuntime::launch(Arc::clone(&owner), session, interpreter, entry)
        .await
        .unwrap();
    let outcome = worker.run(job).await.unwrap();
    worker.shutdown().await.unwrap();
    assert_eq!(outcome.status, "SUCCEEDED");
    let report = read_image_quality(&owner, session, dataset, asset, task).unwrap();
    assert_eq!(
        (report.image.width, report.image.height),
        (Some(2), Some(2))
    );
    let metrics = report.measurements.as_ref().unwrap();
    assert_eq!(metrics.visible_pixels, 4);
    assert_eq!(metrics.dark_fraction, Some(1.0));
    assert_eq!(metrics.gradient_energy, Some(0.0));
    assert_eq!(report.checks[3].assessment, "FINDINGS");
    assert_eq!(report.checks[3].check_id, "image.blur");
    assert!(read_image_quality(&owner, session, dataset, Uuid::new_v4(), task).is_err());
    assert!(read_image_quality(&owner, session, Uuid::new_v4(), asset, task).is_err());
    assert_eq!(fs::read(&image).unwrap(), original);
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(read_image_quality(&owner, session, dataset, asset, task).is_err());
    assert_eq!(
        read_image_quality(&owner, reopened, dataset, asset, task).unwrap(),
        report
    );
    let reported = root
        .join(outcome.output_manifest_path.unwrap())
        .with_file_name("image-quality.json");
    fs::write(reported, b"{}").unwrap();
    assert!(read_image_quality(&owner, reopened, dataset, asset, task).is_err());
    owner.close(reopened).unwrap();
}
