use dd_infra::video_registration::{VideoImport, VideoRegistry};
use dd_infra::video_runtime::VideoProbeRuntime;
use dd_infra::workspace_thread::WorkspaceThread;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
#[path = "video_fixture.rs"]
pub(crate) mod video_fixture;

pub struct Fixture {
    pub root: PathBuf,
    pub source: PathBuf,
    pub original: Vec<u8>,
    pub owner: Arc<WorkspaceThread>,
    pub session: Uuid,
    pub video_id: Uuid,
    pub python: PathBuf,
    pub entry: PathBuf,
}

impl Fixture {
    pub async fn create() -> Self {
        Self::create_with("Registered two color clip.mp4", video_fixture::make_clip).await
    }

    pub async fn create_with(file_name: &str, build: fn(&Path)) -> Self {
        let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
            .canonicalize()
            .unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let python = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
        Self::create_at(&run, &repo, python, file_name, build).await
    }

    pub async fn create_at(
        run: &Path,
        repo: &Path,
        python: PathBuf,
        file_name: &str,
        build: fn(&Path),
    ) -> Self {
        let run = run.join(format!("frame-fixture-{}", Uuid::new_v4()));
        fs::create_dir(&run).unwrap();
        let root = run.join("Frame resolve Workspace");
        fs::create_dir(&root).unwrap();
        let source = run.join(file_name);
        build(&source);
        let original = fs::read(&source).unwrap();
        let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
        let folder = owner.grant_selected_directory(root.clone()).unwrap();
        let workspace = owner
            .create(folder.grant_id, "Frame resolve".into(), 100)
            .unwrap();
        let session = workspace.workspace_session_id;
        let grant = owner.grant_video_file(session, source.clone()).unwrap();
        let registered = VideoRegistry::new(Arc::clone(&owner))
            .unwrap()
            .admit(
                session,
                VideoImport {
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                    file_grant_id: grant.grant_id,
                    display_name: None,
                    stream_index: None,
                },
                200,
            )
            .unwrap();
        let entry = repo
            .join("services/python-worker/src/dd_worker/entrypoint.py")
            .canonicalize()
            .unwrap();
        let mut probe =
            VideoProbeRuntime::launch(Arc::clone(&owner), session, python.clone(), entry.clone())
                .await
                .unwrap();
        assert_eq!(probe.run(registered).await.unwrap().status, "SUCCEEDED");
        probe.shutdown().await.unwrap();

        Self {
            root,
            source,
            original,
            owner,
            session,
            video_id: registered.video_id,
            python,
            entry,
        }
    }
}
