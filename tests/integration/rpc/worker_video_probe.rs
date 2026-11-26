use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use dd_domain::video::SeekPolicy;
use dd_infra::artifact_manifest::TaskManifestScope;
use dd_infra::video_index::read_video_index;
use dd_infra::worker_process::WorkerProcessError;
use dd_protocol::v1::{VideoProbeInput, VideoProbeRequest};
use serde_json::json;
use tonic::Code;
use uuid::Uuid;

#[path = "support/worker_task.rs"]
mod worker_task;
use worker_task::{WorkerTask, digest, file_locator, run_root};

const CAPABILITY: &str = "video_probe.v1";
const DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0";

#[tokio::test]
async fn granted_worker_probes_real_frames_and_core_resolves_normalized_time() {
    let run = run_root();
    let source = run.join("source.mp4");
    let ffmpeg = PathBuf::from(std::env::var_os("DD_WORKER_VIDEO_FFMPEG").unwrap());
    make_clip(&ffmpeg, &source);
    let bytes = fs::read(&source).unwrap();
    let source_digest = digest(&bytes);
    let video_id = Uuid::new_v4();
    let task = WorkerTask::new(
        "Video Probe",
        "VIDEO_PROBE",
        CAPABILITY,
        video_id,
        &source_digest,
        json!({"stream_index": 0, "max_frames": 10}),
    );
    let input = VideoProbeInput {
        source_video: Some(file_locator(&source, &bytes)),
        input_manifest: Some(task.manifest_locator()),
        output_directory: task.output.to_str().unwrap().into(),
        stream_index: Some(0),
        max_pixels: 48 * 32,
        max_frames: 10,
        timeout_ms: 10_000,
    };
    let (context, manifest_ref) = task.dispatch(DOMAIN, &input);
    let mut live = task.launch().await;
    let request = VideoProbeRequest {
        context: Some(context),
        input_manifest_ref: Some(manifest_ref),
        parameters: Some(input),
    };
    let mut altered = request.clone();
    altered.parameters.as_mut().unwrap().stream_index = None;
    let rejected = live.worker.start_video_probe(altered).await.unwrap_err();
    assert!(
        matches!(&rejected, WorkerProcessError::Rpc(status) if status.code() == Code::PermissionDenied),
        "{rejected:?}",
    );
    assert!(fs::read_dir(&task.output).unwrap().next().is_none());
    let handle = live
        .worker
        .start_video_probe(request.clone())
        .await
        .unwrap();
    assert_eq!(
        live.worker.start_video_probe(request).await.unwrap(),
        handle
    );
    let completed = live.wait(&handle, Duration::from_secs(15)).await;
    assert_eq!(completed.status, "SUCCEEDED", "{completed:?}");
    assert_eq!(task.outcome().status, "SUCCEEDED");
    assert_eq!(fs::read(&source).unwrap(), bytes);

    let scope = TaskManifestScope {
        task: task.task_id,
        attempt: Uuid::parse_str(&handle.attempt_id).unwrap(),
        input_fingerprint: task.fingerprint,
    };
    let reference = completed.result_manifest_ref.as_ref().unwrap();
    let read = |stream| {
        read_video_index(
            &task.output,
            reference,
            &scope,
            video_id,
            source_digest,
            Some(stream),
        )
    };
    let index = read(0).unwrap();
    assert!(read(1).is_err());
    let first = index.resolve_index(source_digest, 0).unwrap();
    assert_eq!((first.source_video_id, first.stream_index), (video_id, 0));
    assert!(first.raw_pts.unwrap() > 0);
    assert_eq!((first.pts_us, first.width, first.height), (Some(0), 48, 32));
    let next = index
        .resolve_time(source_digest, 100_001, SeekPolicy::AtOrAfter)
        .unwrap();
    assert_eq!((next.frame_index, next.pts_us), (2, Some(200_000)));
    assert!(index.resolve_index(source_digest, 5).is_err());
    assert!(index.resolve_index([0; 32], 0).is_err());
    let index_path = task.output.join("video-index.json");
    let mut tampered = fs::read(&index_path).unwrap();
    tampered.push(b'\n');
    fs::write(index_path, tampered).unwrap();
    assert!(read(0).is_err());
    live.shutdown().await;
}

fn make_clip(ffmpeg: &Path, output: &Path) {
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=48x32:r=10:d=0.5",
            "-c:v",
            "mpeg4",
            "-bf",
            "2",
            "-threads",
            "1",
            "-output_ts_offset",
            "2",
        ])
        .arg(output);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    assert!(command.status().unwrap().success());
}
