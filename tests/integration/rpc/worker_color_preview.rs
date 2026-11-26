use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dd_core::execution_grant::{GrantBook, GrantScope};
use dd_infra::execution_rpc::CoreClaimService;
use dd_infra::task_store::TaskSubmission;
use dd_infra::worker_process::{
    CoreClaimEndpoint, WorkerLaunch, WorkerProcess, WorkerProcessError,
};
use dd_infra::workspace_thread::WorkspaceThread;
use dd_protocol::v1::{
    ArtifactRef, ColorMethod, ColorOperation, ColorPoint, ColorPreviewInput, ColorPreviewRequest,
    ColorRectangle, ExecutionLookup, TaskContext, WorkerInputFile, artifact_ref,
    color_preview_input, core_execution_control_server::CoreExecutionControlServer,
};
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    sync::oneshot,
    time::{sleep, timeout},
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Code;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(10);
const CAPABILITY: &str = "color_preview.v1";
const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[tokio::test]
async fn real_color_preview_requires_its_grant_and_seals_pixels_without_modifying_the_source() {
    let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let root = run.join("Color Preview Workspace");
    fs::create_dir(&root).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, TIMEOUT).unwrap());
    let directory = thread.grant_selected_directory(root.clone()).unwrap();
    let workspace = thread
        .create(directory.grant_id, "Color".into(), 100)
        .unwrap();
    let session = workspace.workspace_session_id;
    let executor = Uuid::new_v4();
    let core_key = Uuid::new_v4();
    let task_fingerprint = [0x62; 32];
    let image = root.join("original.png");
    let manifest_path = root.join("input.json");
    let manifest = serde_json::to_vec(&json!({
        "dataset_id": null, "label_schema_digest": null,
        "assets": [{"asset_id": Uuid::new_v4(), "media_digest": digest(PNG), "annotation_digest": null, "source_metadata_digest": null}],
        "resolved_config": {"max_pixels": 2}, "capability_id": CAPABILITY,
        "implementation_version": "0.1.0", "model_identity": null,
        "input_fingerprint": "62".repeat(32),
    })).unwrap();
    fs::write(&image, PNG).unwrap();
    fs::write(&manifest_path, &manifest).unwrap();
    let task = thread
        .enqueue_task(
            TaskSubmission {
                dataset_id: None,
                workspace_session: session,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                operation_kind: "COLOR_PREVIEW".into(),
                capability_id: CAPABILITY.into(),
                input_manifest_path: "input.json".into(),
                request_fingerprint: [0x72; 32],
                input_fingerprint: task_fingerprint,
            },
            200,
        )
        .unwrap();
    let attempt = Uuid::new_v4();
    let output = root.join(format!("artifacts/tasks/{}/{attempt}", task.task_id));
    fs::create_dir_all(&output).unwrap();
    let input = ColorPreviewInput {
        image: Some(locator(&image, PNG)),
        input_manifest: Some(locator(&manifest_path, &manifest)),
        output_directory: output.to_str().unwrap().into(),
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
    let mut operation_digest = Sha256::new();
    operation_digest.update(b"datasetdoctor.v1.PythonWorker.StartColorPreview\0");
    operation_digest.update(input.encode_to_vec());
    let scope = GrantScope {
        workspace_session: session,
        task: task.task_id,
        attempt,
        executor_session: executor,
        input_fingerprint: task_fingerprint,
        operation_digest: operation_digest.finalize().into(),
        reservation: Uuid::new_v4(),
    };
    let mut grants = GrantBook::new(session);
    let grant = grants
        .install(scope.clone(), Instant::now() + TIMEOUT)
        .unwrap();
    thread
        .record_dispatch(scope, grant, [0x91; 32], 250)
        .unwrap();
    let core = CoreClaimService::new(
        executor,
        core_key,
        Arc::new(Mutex::new(grants)),
        Arc::clone(&thread),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let SocketAddr::V4(address) = listener.local_addr().unwrap() else {
        panic!("expected IPv4")
    };
    let (stop, stopping) = oneshot::channel();
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(CoreExecutionControlServer::new(core))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = stopping.await;
            })
            .await
            .unwrap();
    });
    let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let mut worker = WorkerProcess::launch(WorkerLaunch {
        interpreter: python.clone(),
        entrypoint: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../services/python-worker/src/dd_worker/entrypoint.py")
            .canonicalize()
            .unwrap(),
        executor_session: executor,
        core: Some(CoreClaimEndpoint {
            address,
            claim_token: core_key,
        }),
    })
    .await
    .unwrap();
    let request = ColorPreviewRequest {
        context: Some(TaskContext {
            workspace_id: workspace.workspace_id,
            workspace_session_id: session.to_string(),
            task_id: task.task_id.to_string(),
            attempt_id: attempt.to_string(),
            executor_session_id: executor.to_string(),
            input_fingerprint: "62".repeat(32),
            node_spec_fingerprint: "91".repeat(32),
            grant_id: grant.to_string(),
            ..Default::default()
        }),
        input_manifest_ref: Some(ArtifactRef {
            artifact_id: Uuid::new_v4().to_string(),
            owner: Some(artifact_ref::Owner::OwnerTaskId(task.task_id.to_string())),
            kind: "INPUT_MANIFEST".into(),
            size_bytes: manifest.len() as u64,
            media_type: "application/json".into(),
            sha256: digest(&manifest),
            schema_id: None,
        }),
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
    let rejected = worker.start_color_preview(altered).await.unwrap_err();
    assert!(
        matches!(&rejected, WorkerProcessError::Rpc(status) if status.code() == Code::PermissionDenied),
        "{rejected:?}"
    );
    assert!(fs::read_dir(&output).unwrap().next().is_none());
    assert!(
        worker
            .health()
            .await
            .unwrap()
            .loaded_capability_ids
            .contains(&CAPABILITY.into())
    );

    let handle = worker.start_color_preview(request.clone()).await.unwrap();
    assert_eq!(worker.start_color_preview(request).await.unwrap(), handle);
    let completed = timeout(TIMEOUT, async {
        loop {
            let status = worker
                .execution(ExecutionLookup {
                    task_id: handle.task_id.clone(),
                    attempt_id: handle.attempt_id.clone(),
                })
                .await
                .unwrap();
            if !matches!(status.status.as_str(), "QUEUED" | "RUNNING") {
                return status;
            }
            sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(completed.status, "SUCCEEDED", "{completed:?}");
    let outcome = thread.task_outcome(session, task.task_id).unwrap();
    assert_eq!(outcome.status, "SUCCEEDED");
    assert_eq!(fs::read(&image).unwrap(), PNG);
    let candidate = output.join("candidate.png");
    let mask = output.join("mask.png");
    let status = Command::new(&python).args(["-I", "-B", "-c",
        "import sys; from PIL import Image; original, candidate, mask = map(Image.open, sys.argv[1:]); assert candidate.getpixel((0,0)) == (17,30,90); assert candidate.getpixel((1,0)) == original.getpixel((1,0)); assert mask.get_flattened_data() == (255, 0)"])
        .args([&image, &candidate, &mask]).status().unwrap();
    assert!(status.success());
    let details: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("color-preview.json")).unwrap()).unwrap();
    assert_eq!(details["base_image_digest"], digest(PNG));
    assert_eq!(
        details["region"],
        json!({"kind": "RECT", "bbox_xywh": [0.0, 0.0, 1.0, 1.0]})
    );
    assert_eq!(details["operation"]["target_rgb"], json!([17, 30, 90]));
    assert_eq!(details["operation"]["strength"], 1);
    assert_eq!(details["changed_pixels"], 1);
    worker.shutdown().await.unwrap();
    stop.send(()).unwrap();
    server.await.unwrap();
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn locator(path: &Path, bytes: &[u8]) -> WorkerInputFile {
    WorkerInputFile {
        path: path.to_str().unwrap().into(),
        sha256: digest(bytes),
    }
}
