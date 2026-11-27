#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::{fs, path::PathBuf, process::Command, sync::Arc, time::Duration};

use dd_infra::video_registration::{VideoImport, VideoRegistry};
use dd_infra::video_runtime::VideoProbeRuntime;
use serde_json::json;
use uuid::Uuid;

#[path = "support/process_handle.rs"]
mod process_handle;
#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)]
mod workflow_fixture;

#[tokio::test]
async fn cancelling_a_real_video_task_reaps_ffmpeg_and_persists_no_result() {
    let fixture = workflow_fixture::WorkflowFixture::with_samples("CancelVideo", br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"signal"}],"annotations":[]}"#, json!({}), &["signal.png"]);
    let ffmpeg = PathBuf::from(std::env::var_os("DD_WORKER_VIDEO_FFMPEG").unwrap());
    let source = fixture.source.join("synthetic.mkv");
    assert!(
        Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-threads",
                "1",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=48x32:r=4:d=1",
                "-c:v",
                "ffv1"
            ])
            .arg(&source)
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    let original = fs::read(&source).unwrap();
    let grant = fixture
        .workspace
        .grant_video_file(fixture.session, source.clone())
        .unwrap();
    let receipt = VideoRegistry::new(Arc::clone(&fixture.workspace))
        .unwrap()
        .admit(
            fixture.session,
            VideoImport {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                file_grant_id: grant.grant_id,
                display_name: None,
                stream_index: Some(0),
            },
            1,
        )
        .unwrap();
    let marker = fixture.root.join("ffmpeg.pid");
    let wrapper = fixture.root.join("blocked-video-worker.py");
    let directory =
        serde_json::to_string(fixture.entrypoint.parent().unwrap().to_str().unwrap()).unwrap();
    fs::write(&wrapper, format!(r#"
import sys, subprocess, os
from pathlib import Path
sys.path.insert(0, {directory})
import entrypoint, video_decoder
original_popen = subprocess.Popen
def popen(*args, **kwargs):
    child = original_popen(*args, **kwargs)
    if Path(args[0][0]) == Path(os.environ['DD_WORKER_VIDEO_FFMPEG']):
        temporary = Path({marker}).with_suffix('.tmp')
        temporary.write_text(str(child.pid), encoding='ascii')
        os.replace(temporary, {marker})
    return child
subprocess.Popen = popen
original_capture = video_decoder.VideoDecoder._capture
def capture(self, executable, args, limit):
    # Keep the real authorized decoder blocked in a real FFmpeg pipe read.
    return original_capture(self, self.ffmpeg, ['-nostdin', '-stream_loop', '-1', '-re', '-i', {source}, '-f', 'null', '-'], 1024)
video_decoder.VideoDecoder._capture = capture
raise SystemExit(entrypoint.main())
"#, marker=serde_json::to_string(marker.to_str().unwrap()).unwrap(),
source=serde_json::to_string(source.to_str().unwrap()).unwrap())).unwrap();
    let mut worker = VideoProbeRuntime::launch(
        Arc::clone(&fixture.workspace),
        fixture.session,
        fixture.interpreter.clone(),
        wrapper,
    )
    .await
    .unwrap();
    let execution = tokio::spawn(async move {
        let result = worker.run(receipt).await;
        if result.is_ok() {
            worker.shutdown().await.unwrap();
        } else {
            worker.terminate().await.unwrap();
        }
        result
    });
    tokio::time::timeout(Duration::from_secs(8), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let child = process_handle::SyntheticProcess::open(
        fs::read_to_string(marker).unwrap().parse().unwrap(),
    );
    assert_eq!(
        fixture
            .workspace
            .cancel_task(fixture.session, receipt.task_id, 5)
            .unwrap()
            .observed_status,
        "CANCELLING"
    );
    let outcome = tokio::time::timeout(Duration::from_secs(8), execution)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.status, "CANCELLED");
    assert!(outcome.output_manifest_path.is_none());
    assert!(
        child.stopped(),
        "FFmpeg pipe owner must be reaped before completion"
    );
    fixture.workspace.close(fixture.session).unwrap();
    let grant = fixture
        .workspace
        .grant_selected_directory(fixture.root.clone())
        .unwrap();
    let reopened = fixture
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert_eq!(
        fixture
            .workspace
            .task_outcome(reopened, receipt.task_id)
            .unwrap()
            .status,
        "CANCELLED"
    );
    assert_eq!(fs::read(source).unwrap(), original);
}
