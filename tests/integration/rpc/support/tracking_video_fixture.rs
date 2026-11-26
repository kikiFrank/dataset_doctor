use std::path::Path;
use std::process::Command;

pub fn make_motion_clip(output: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new(std::env::var_os("DD_WORKER_PYTHON").unwrap())
        .args(["-I", "-B"])
        .arg(repo.join("tests/worker/video/tracking_fixture.py"))
        .arg(output)
        .arg(std::env::var_os("DD_WORKER_VIDEO_FFMPEG").unwrap())
        .status()
        .unwrap();
    assert!(status.success());
}
