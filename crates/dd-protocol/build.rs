use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-env-changed=PROTOC");
    let protoc = env::var_os("PROTOC").unwrap_or_else(|| "protoc".into());
    let version = Command::new(&protoc).arg("--version").output()?;
    if !version.status.success() || std::str::from_utf8(&version.stdout)?.trim() != "libprotoc 29.3"
    {
        return Err("dd-protocol requires libprotoc 29.3; set PROTOC to its executable".into());
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/proto");
    let sources = [
        root.join("datasetdoctor/v1/common.proto"),
        root.join("datasetdoctor/v1/core_execution.proto"),
        root.join("datasetdoctor/v1/worker_service.proto"),
    ];
    for source in &sources {
        println!("cargo::rerun-if-changed={}", source.display());
    }
    let mut prost = tonic_prost_build::Config::new();
    prost.protoc_executable(protoc);
    tonic_prost_build::configure().compile_with_config(prost, &sources, &[root])?;
    Ok(())
}
