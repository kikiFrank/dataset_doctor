mod support;

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use dd_infra::workspace::{WorkspaceLock, WorkspaceLockError};

const FILE_NAME: &str = ".datasetdoctor.lock";
const CHILD_ROOT: &str = "DD_WORKSPACE_TEST_CHILD_ROOT";
const CHILD_EXPECTS: &str = "DD_WORKSPACE_TEST_CHILD_EXPECTS";
const CASE: &str = "workspace_writer_is_exclusive_across_processes";

#[test]
fn workspace_writer_is_exclusive_across_processes() {
    if let Some(path) = env::var_os(CHILD_ROOT) {
        let result = WorkspaceLock::acquire(Path::new(&path));
        match env::var(CHILD_EXPECTS).unwrap().as_str() {
            "busy" => assert!(matches!(result, Err(WorkspaceLockError::Busy))),
            "available" => assert!(result.is_ok(), "{result:?}"),
            other => panic!("unknown test child role {other}"),
        }
        return;
    }

    let workspace = support::workspace_root("工作区 synthetic");
    let holder = WorkspaceLock::acquire(&workspace).unwrap();
    assert!(workspace.join(FILE_NAME).is_file());
    run_contender(&workspace, "busy");
    #[cfg(windows)]
    assert!(
        fs::remove_file(workspace.join(FILE_NAME)).is_err(),
        "a contender could replace the lock file while it was held"
    );
    drop(holder);
    assert!(
        workspace.join(FILE_NAME).is_file(),
        "lock file identity should survive release"
    );
    run_contender(&workspace, "available");
}

fn run_contender(workspace: &Path, expected: &str) {
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", CASE, "--nocapture"])
        .env(CHILD_ROOT, workspace)
        .env(CHILD_EXPECTS, expected)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "contender expected {expected}: {status}");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("contender did not return within the deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
