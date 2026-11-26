use std::path::{Path, PathBuf};

use dd_infra::worker_process::{WorkerLaunch, WorkerProcess, WorkerProcessError};
use dd_protocol::v1::ExecutionLookup;
use tonic::Code;
use uuid::Uuid;

#[tokio::test]
async fn managed_worker_bootstraps_authenticates_and_exits_cleanly() {
    let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let entrypoint = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/python-worker/src/dd_worker/entrypoint.py")
        .canonicalize()
        .unwrap();
    let executor_session = Uuid::new_v4();
    let mut worker = WorkerProcess::launch(WorkerLaunch {
        interpreter,
        entrypoint,
        executor_session,
        core: None,
    })
    .await
    .unwrap();
    let health = worker.health().await.unwrap();
    assert_eq!(health.role, "WORKER");
    assert_eq!(health.executor_session_id, executor_session.to_string());
    assert_eq!(health.protocol_major, dd_protocol::PROTOCOL_MAJOR);
    assert!(health.loaded_capability_ids.is_empty());
    let lookup = worker
        .execution(ExecutionLookup {
            task_id: Uuid::new_v4().to_string(),
            attempt_id: Uuid::new_v4().to_string(),
        })
        .await
        .unwrap_err();
    assert!(matches!(lookup, WorkerProcessError::Rpc(status) if status.code() == Code::NotFound));
    worker.shutdown().await.unwrap();
}
