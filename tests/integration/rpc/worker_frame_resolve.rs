use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use dd_infra::artifact_manifest::TaskManifestScope;
use dd_protocol::v1::{FrameResolveInput, FrameResolveRequest, VideoProbeInput, VideoProbeRequest};
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "support/worker_task.rs"]
mod worker_task;
use worker_task::{WorkerTask, digest, file_locator, run_root};

const PROBE_DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0";
const RESOLVE_DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartFrameResolve\0";

#[tokio::test]
async fn worker_decodes_the_exact_frame_selected_from_the_core_index() {
    let run = run_root();
    let source = run.join("red-and-blue.mp4");
    make_clip(&source);
    let original = fs::read(&source).unwrap();
    let sha = digest(&original);
    let video_id = Uuid::new_v4();
    let probe = WorkerTask::new(
        "Probe for Resolve",
        "VIDEO_PROBE",
        "video_probe.v1",
        video_id,
        &sha,
        json!({"stream_index": 0}),
    );
    let input = VideoProbeInput {
        source_video: Some(file_locator(&source, &original)),
        input_manifest: Some(probe.manifest_locator()),
        output_directory: probe.output.to_str().unwrap().into(),
        stream_index: Some(0),
        max_pixels: 48 * 32,
        max_frames: 10,
        timeout_ms: 10_000,
    };
    let (context, manifest) = probe.dispatch(PROBE_DOMAIN, &input);
    let mut worker = probe.launch().await;
    let handle = worker
        .worker
        .start_video_probe(VideoProbeRequest {
            context: Some(context),
            input_manifest_ref: Some(manifest),
            parameters: Some(input),
        })
        .await
        .unwrap();
    let outcome = worker.wait(&handle, Duration::from_secs(15)).await;
    assert_eq!(outcome.status, "SUCCEEDED", "{outcome:?}");
    let index = dd_infra::video_index::read_video_index(
        &probe.output,
        outcome.result_manifest_ref.as_ref().unwrap(),
        &scope(&probe, &handle.attempt_id),
        video_id,
        sha,
        Some(0),
    )
    .unwrap();
    let frame = index.resolve_index(sha, 3).unwrap();
    assert_eq!(frame.pts_us, Some(600_000));
    worker.shutdown().await;

    for (case, raw_pts) in [
        ("Resolve Exact Frame", frame.raw_pts),
        ("Reject PTS Drift", Some(0)),
    ] {
        let task = WorkerTask::new(
            case,
            "FRAME_RESOLVE",
            "frame_resolve.v1",
            video_id,
            &sha,
            json!({"stream_index": frame.stream_index, "frame_index": frame.frame_index.to_string()}),
        );
        let input = FrameResolveInput {
            source_video: Some(file_locator(&source, &original)),
            input_manifest: Some(task.manifest_locator()),
            output_directory: task.output.to_str().unwrap().into(),
            stream_index: frame.stream_index,
            frame_index: Some(frame.frame_index),
            expected_raw_pts: raw_pts,
            display_width: frame.width,
            display_height: frame.height,
            max_pixels: 48 * 32,
            max_frames: 10,
            timeout_ms: 10_000,
        };
        let (context, manifest) = task.dispatch(RESOLVE_DOMAIN, &input);
        let mut worker = task.launch().await;
        let handle = worker
            .worker
            .start_frame_resolve(FrameResolveRequest {
                context: Some(context),
                input_manifest_ref: Some(manifest),
                parameters: Some(input),
            })
            .await
            .unwrap();
        let outcome = worker.wait(&handle, Duration::from_secs(15)).await;
        if raw_pts == frame.raw_pts {
            assert_eq!(outcome.status, "SUCCEEDED", "{outcome:?}");
            assert_eq!(task.outcome().status, "SUCCEEDED");
            let scope = scope(&task, &handle.attempt_id);
            let manifest = outcome.result_manifest_ref.as_ref().unwrap();
            let attach = |frame| {
                dd_infra::video_index::read_resolved_video_frame(
                    &task.output,
                    manifest,
                    &scope,
                    frame,
                )
            };
            let image = attach(frame).unwrap();
            assert_eq!(image.frame, frame);
            assert_eq!(image.reference.kind, "VIDEO_FRAME_IMAGE");
            assert_eq!(image.reference.media_type, "image/png");
            assert!(attach(dd_domain::video::FrameRef { width: 49, ..frame }).is_err());
            let metadata: Value =
                serde_json::from_slice(&fs::read(task.output.join("frame.json")).unwrap()).unwrap();
            assert_eq!(metadata["source_video_id"], video_id.to_string());
            assert_eq!(metadata["frame_index"], "3");
            assert_eq!(metadata["raw_pts"], frame.raw_pts.unwrap().to_string());
            let status = Command::new(std::env::var_os("DD_WORKER_PYTHON").unwrap())
                .args(["-I", "-B", "-c",
                    "import sys; from PIL import Image; im=Image.open(sys.argv[1]); r,g,b=im.getpixel((20,12)); assert im.size==(48,32) and b>240 and max(r,g)<12"])
                .arg(task.output.join(&image.relative_path)).status().unwrap();
            assert!(status.success());
        } else {
            assert_eq!(outcome.status, "FAILED");
            assert_eq!(outcome.error_code, "input_changed");
            assert!(fs::read_dir(&task.output).unwrap().next().is_none());
        }
        worker.shutdown().await;
    }
    assert_eq!(fs::read(source).unwrap(), original);
}

fn scope(task: &WorkerTask, attempt: &str) -> TaskManifestScope {
    TaskManifestScope {
        task: task.task_id,
        attempt: Uuid::parse_str(attempt).unwrap(),
        input_fingerprint: task.fingerprint,
    }
}

fn make_clip(output: &Path) {
    let ffmpeg = PathBuf::from(std::env::var_os("DD_WORKER_VIDEO_FFMPEG").unwrap());
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
            "color=red:s=48x32:r=5:d=0.4",
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=48x32:r=5:d=0.4",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0[clip]",
            "-map",
            "[clip]",
            "-c:v",
            "mpeg4",
            "-bf",
            "1",
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
