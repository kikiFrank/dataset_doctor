use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub fn workspace_root(case: &str) -> PathBuf {
    let run = PathBuf::from(
        env::var_os("DD_WORKSPACE_LOCK_RUN_DIR").expect("set the Workspace test run directory"),
    )
    .canonicalize()
    .unwrap();
    let runs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/p02-workspace/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(runs.as_path()));
    let root = run.join(case);
    fs::create_dir(&root).unwrap();
    root
}
