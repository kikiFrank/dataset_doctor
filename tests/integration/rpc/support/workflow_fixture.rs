use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dd_core::workflow::{PlanPolicy, ScopeKind, Step, WorkflowInput};
use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::input_fingerprint::freeze_input_manifest;
use dd_infra::json::JsonContracts;
use dd_infra::workflow_store::{
    WorkflowDraftRequest, WorkflowProfileRequest, WorkflowSnapshot, WorkflowStore,
};
use dd_infra::workspace_thread::WorkspaceThread;
use serde_json::{Value, json};
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../../testdata/image_io/p00-worker-rgb.png");

pub struct WorkflowFixture {
    pub root: PathBuf,
    pub source: PathBuf,
    pub workspace: Arc<WorkspaceThread>,
    pub session: Uuid,
    pub dataset: Uuid,
    pub policy: PlanPolicy,
    pub store: WorkflowStore,
    pub ready: WorkflowSnapshot,
    pub interpreter: PathBuf,
    pub entrypoint: PathBuf,
}

impl WorkflowFixture {
    pub fn new(case: &str, annotation: &[u8]) -> Self {
        Self::with_config(case, annotation, json!({}))
    }

    pub fn with_config(case: &str, annotation: &[u8], config: Value) -> Self {
        Self::with_samples(case, annotation, config, &["signal.png", "unannotated.png"])
    }

    pub fn with_samples(case: &str, annotation: &[u8], config: Value, names: &[&str]) -> Self {
        let run = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
            .canonicalize()
            .unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        assert_eq!(
            run.parent(),
            Some(repo.join("target/contract-check/worker-rpc").as_path())
        );
        let root = run.join(format!("{case} Workspace"));
        let source = run.join(format!("{case} Sources"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&source).unwrap();
        for name in names {
            fs::write(source.join(name), PNG).unwrap();
        }
        fs::write(source.join("signal.json"), annotation).unwrap();
        let workspace = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
        let grant = workspace.grant_selected_directory(root.clone()).unwrap();
        let session = workspace
            .create(grant.grant_id, case.into(), 1)
            .unwrap()
            .workspace_session_id;
        let imports = DatasetImportCoordinator::new(Arc::clone(&workspace)).unwrap();
        let grant = workspace
            .grant_import_directory(session, source.clone())
            .unwrap();
        let inspected = imports
            .inspect_directory(session, grant.grant_id, "Workflow samples".into())
            .unwrap();
        let registered = imports
            .confirm(session, inspected.plan_id, inspected.fingerprint, 2)
            .unwrap();
        let dataset = registered.dataset_id.parse().unwrap();
        let members = workspace
            .list_assets(session, dataset, None, 10)
            .unwrap()
            .items;
        assert_eq!(members.len(), names.len());
        let assets: Vec<_> = members
            .iter()
            .map(|member| {
                let image = workspace
                    .registered_image(session, dataset, member.asset_id)
                    .unwrap();
                json!({"asset_id": member.asset_id, "media_digest": image.image.sha256,
                "annotation_digest": image.annotation.as_ref().map(|file| &file.sha256),
                "source_metadata_digest":null})
            })
            .collect();
        let datasets = workspace.list_datasets(session, None, 1).unwrap();
        let frozen = freeze_input_manifest(
            &JsonContracts::new().unwrap(),
            json!({
                "dataset_id":dataset, "label_schema_digest":datasets.items[0].schema_fingerprint,
                "assets":assets, "resolved_config":config, "implementation_version":"1",
                "capability_id":"workflow_plan_preview.v1", "model_identity":null,
            }),
        )
        .unwrap();
        let input_path = PathBuf::from("artifacts/task-inputs/workflow-plan.json");
        fs::create_dir_all(root.join(input_path.parent().unwrap())).unwrap();
        fs::write(root.join(&input_path), frozen.bytes).unwrap();
        let store = WorkflowStore::new(Arc::clone(&workspace)).unwrap();
        let draft = store
            .admit(
                session,
                WorkflowDraftRequest {
                    dataset_id: dataset,
                    source: WorkflowInput {
                        kind: ScopeKind::Dataset,
                        manifest_id: Uuid::new_v4(),
                        fingerprint: frozen.input_fingerprint,
                    },
                    template_id: "industrial_dataset_healthcheck.v1".into(),
                    input_path,
                    input_manifest_sha256: frozen.sha256,
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                },
                3,
            )
            .unwrap();
        let policy = PlanPolicy {
            available_steps: BTreeSet::from([Step::DatasetProfile, Step::AnnotationGeometry]),
            allowed_model_profiles: BTreeSet::new(),
            max_sample_limit: 10,
            max_adjustments: 0,
            max_model_calls: 0,
            max_output_tokens: 0,
        };
        let ready = store
            .accept_default(
                session,
                draft.run_id,
                draft.row_revision,
                &policy,
                names.len() as u32,
                4,
            )
            .unwrap();
        let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
        let entrypoint = repo.join("services/python-worker/src/dd_worker/entrypoint.py");
        Self {
            root,
            source,
            workspace,
            session,
            dataset,
            policy,
            store,
            ready,
            interpreter,
            entrypoint,
        }
    }

    pub fn start_request(&self) -> WorkflowProfileRequest {
        WorkflowProfileRequest {
            reference: self.ready.plan_ref.clone().unwrap(),
            expected_row_revision: self.ready.row_revision,
            request_id: Uuid::new_v4(),
            idempotency_key: Uuid::new_v4(),
            max_pixels: 100,
        }
    }
}
