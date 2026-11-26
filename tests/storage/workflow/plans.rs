use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use dd_core::workflow::{
    ApprovalGate, DefaultSkipReason, EnabledStep, PlanPolicy, ScopeKind, Step, TemplatePlan,
    WorkflowInput,
};
use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::input_fingerprint::freeze_input_manifest;
use dd_infra::json::JsonContracts;
use dd_infra::workflow_store::{
    PlanSource, WorkflowDraftRequest, WorkflowError, WorkflowStatus, WorkflowStore,
};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

const TEMPLATE: &str = "industrial_dataset_healthcheck.v1";

#[test]
fn plans_bind_the_current_input_and_reopen_the_same_accepted_run() {
    let run = PathBuf::from(
        std::env::var_os("DD_SQLITE_MIGRATION_RUN_DIR").expect("run storage acceptance script"),
    )
    .canonicalize()
    .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(run.starts_with(repo.join("target/p01-04/runs")));
    let root = run.join(format!("workflow-{}", Uuid::new_v4()));
    let source = root.join("source");
    let workspace = root.join("Workspace");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&source).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(
        source.join("frame.png"),
        include_bytes!("../../../testdata/image_io/p00-worker-rgb.png"),
    )
    .unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(workspace.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Workflow".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Images".into())
        .unwrap();
    let receipt = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap();
    let dataset = Uuid::parse_str(&receipt.dataset_id).unwrap();
    let datasets = owner.list_datasets(session, None, 1).unwrap();
    let image = owner.list_assets(session, dataset, None, 1).unwrap().items[0].asset_id;
    let binding = owner.registered_image(session, dataset, image).unwrap();
    let frozen = freeze_input_manifest(&JsonContracts::new().unwrap(), json!({
        "dataset_id": dataset.to_string(), "label_schema_digest": datasets.items[0].schema_fingerprint,
        "assets": [{"asset_id": image.to_string(), "media_digest": binding.image.sha256,
            "annotation_digest": null, "source_metadata_digest": null}],
        "resolved_config": {}, "implementation_version": "1",
        "capability_id": "workflow_plan_preview.v1", "model_identity": null,
    })).unwrap();
    let relative = PathBuf::from("artifacts/task-inputs/planning.json");
    fs::create_dir_all(workspace.join("artifacts/task-inputs")).unwrap();
    fs::write(workspace.join(&relative), &frozen.bytes).unwrap();
    let input = WorkflowInput {
        kind: ScopeKind::Dataset,
        manifest_id: Uuid::new_v4(),
        fingerprint: frozen.input_fingerprint,
    };
    let policy = PlanPolicy {
        available_steps: BTreeSet::from([Step::DatasetProfile, Step::AnnotationGeometry]),
        allowed_model_profiles: BTreeSet::new(),
        max_sample_limit: 100,
        max_adjustments: 1,
        max_model_calls: 0,
        max_output_tokens: 0,
    };
    let proposal = TemplatePlan {
        template_id: TEMPLATE.into(),
        template_contract_version: 1,
        input_manifest_id: input.manifest_id,
        expected_scope_fingerprint: input.fingerprint,
        enabled_steps: vec![
            EnabledStep::DatasetProfile { sample_limit: 1 },
            EnabledStep::AnnotationGeometry { sample_limit: 1 },
        ],
        max_adjustments: 1,
        required_review_gates: vec![ApprovalGate::FinalAnnotation],
        allowed_fallbacks: BTreeMap::new(),
    };
    let request = || WorkflowDraftRequest {
        dataset_id: dataset,
        source: WorkflowInput {
            kind: input.kind,
            manifest_id: input.manifest_id,
            fingerprint: input.fingerprint,
        },
        template_id: TEMPLATE.into(),
        input_path: relative.clone(),
        input_manifest_sha256: frozen.sha256,
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::nil(),
    };
    let store = WorkflowStore::new(Arc::clone(&owner)).unwrap();
    let key = Uuid::new_v4();
    let fresh = || WorkflowDraftRequest {
        idempotency_key: key,
        ..request()
    };
    let draft = store.admit(session, fresh(), 3).unwrap();
    assert_eq!(draft.status, WorkflowStatus::Draft);
    assert_eq!(
        owner
            .task_outcome(session, draft.planning_task_id)
            .unwrap()
            .status,
        "QUEUED"
    );
    assert_eq!(
        store.admit(session, fresh(), 4).unwrap().run_id,
        draft.run_id
    );
    let mut invalid = proposal.clone();
    invalid.required_review_gates.clear();
    assert!(matches!(
        store.accept(
            session,
            draft.run_id,
            draft.row_revision,
            &policy,
            invalid,
            4
        ),
        Err(WorkflowError::Plan(_))
    ));
    assert_eq!(
        store.get(session, draft.run_id).unwrap().status,
        WorkflowStatus::Draft
    );
    let db = Connection::open(workspace.join("workspace.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_plan_receipt BEFORE UPDATE OF status ON tasks
         WHEN NEW.operation_kind='WORKFLOW_PLAN_PREVIEW' AND NEW.status='SUCCEEDED'
         BEGIN SELECT RAISE(ABORT, 'synthetic plan receipt failure'); END;",
    )
    .unwrap();
    let failure = store
        .accept(
            session,
            draft.run_id,
            draft.row_revision,
            &policy,
            proposal.clone(),
            5,
        )
        .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("synthetic plan receipt failure"),
        "{failure}"
    );
    assert_eq!(
        store.get(session, draft.run_id).unwrap().status,
        WorkflowStatus::Draft
    );
    assert_eq!(
        owner
            .task_outcome(session, draft.planning_task_id)
            .unwrap()
            .status,
        "QUEUED"
    );
    db.execute_batch("DROP TRIGGER fail_plan_receipt").unwrap();
    drop(db);
    let ready = store
        .accept(
            session,
            draft.run_id,
            draft.row_revision,
            &policy,
            proposal,
            6,
        )
        .unwrap();
    assert_eq!(ready.status, WorkflowStatus::Ready);
    assert_eq!(ready.planning_task_id, draft.planning_task_id);
    assert_eq!(
        owner
            .task_outcome(session, ready.planning_task_id)
            .unwrap()
            .status,
        "SUCCEEDED"
    );
    let reference = ready.plan_ref.unwrap();
    assert_eq!(reference.plan_revision, 1);
    assert_eq!(reference.input_fingerprint, input.fingerprint);
    let local_request = WorkflowDraftRequest {
        idempotency_key: Uuid::new_v4(),
        ..request()
    };
    let local_draft = store.admit(session, local_request, 7).unwrap();
    let default = store
        .accept_default(
            session,
            local_draft.run_id,
            local_draft.row_revision,
            &policy,
            1,
            8,
        )
        .unwrap();
    assert_eq!(default.status, WorkflowStatus::Ready);
    assert_eq!(default.plan_source, Some(PlanSource::LocalDefault));
    assert_eq!(
        default.not_executed.get(&Step::VisualReview),
        Some(&DefaultSkipReason::ModelNotConfigured)
    );
    assert_eq!(
        owner
            .task_outcome(session, default.planning_task_id)
            .unwrap()
            .status,
        "SUCCEEDED"
    );
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(workspace.clone()).unwrap();
    let session = owner.open(grant.grant_id).unwrap().workspace_session_id;
    let recovered = store.get(session, draft.run_id).unwrap();
    assert_eq!(recovered.status, WorkflowStatus::Ready);
    assert_eq!(recovered.plan_source, Some(PlanSource::Proposed));
    assert_eq!(recovered.plan_ref.as_ref(), Some(&reference));
    assert!(store.validate_ready(session, &reference, &policy).is_ok());
    let local = store.get(session, local_draft.run_id).unwrap();
    assert_eq!(local.plan_source, Some(PlanSource::LocalDefault));
    assert_eq!(local.not_executed, default.not_executed);
    let live = source.join("frame.png");
    let before = fs::read(&live).unwrap();
    fs::write(&live, b"changed media").unwrap();
    assert!(matches!(
        store.validate_ready(session, &reference, &policy),
        Err(WorkflowError::InputChanged)
    ));
    fs::write(&live, before).unwrap();

    fs::write(workspace.join(relative), b"changed input manifest").unwrap();
    assert!(matches!(
        store.validate_ready(session, &reference, &policy),
        Err(WorkflowError::InputChanged)
    ));
    assert_eq!(
        store.get(session, draft.run_id).unwrap().status,
        WorkflowStatus::Ready
    );
    owner.close(session).unwrap();
}
