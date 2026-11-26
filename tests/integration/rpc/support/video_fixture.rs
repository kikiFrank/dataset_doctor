use std::path::{Path, PathBuf};
use std::process::Command;

pub fn make_clip(output: &Path) {
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
