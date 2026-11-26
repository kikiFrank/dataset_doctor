use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use dd_infra::coco_export::{
    CocoExportPolicy, CocoExportRequest, CocoExportService, ExclusionReason,
};
use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::dataset_query::AssetAnnotationState;
use dd_infra::geometry_runtime::{GeometryAuditRequest, GeometryRuntime};
use dd_infra::review_draft::{ReviewDraftRequest, ReviewDraftService};
use dd_infra::review_final::{FinalOperationState, ReviewFinalRequest, ReviewFinalService};
use dd_infra::workspace_thread::WorkspaceThread;
use serde_json::{Value, json};
use uuid::Uuid;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[tokio::test]
async fn approved_finals_export_to_the_actual_coco_reader_with_local_ids_reassigned() {
    let runs = PathBuf::from(std::env::var_os("DD_CORE_CLAIM_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert_eq!(
        runs.parent(),
        Some(
            repository
                .join("target/contract-check/worker-rpc")
                .as_path()
        )
    );
    let run = runs.join(format!("export-{}", Uuid::new_v4()));
    let root = run.join("Workspace");
    let source = run.join("Source");
    let destination = run.join("Export");
    fs::create_dir(&run).unwrap();
    for directory in [&root, &source, &destination] {
        fs::create_dir(directory).unwrap();
    }
    let original = |name: &str, negative: bool| {
        let (categories, annotations) = if negative {
            (
                json!([{"id": 90, "name": "helmet"}, {"id": 2, "name": "signal"}]),
                json!([]),
            )
        } else {
            (
                json!([{"id": 12, "name": "signal"}, {"id": 8, "name": "helmet"}]),
                json!([
                    {"id": 9, "image_id": 1, "category_id": 12, "bbox": [0,0,1,1], "area": 1, "iscrowd": 0},
                    {"id": 2, "image_id": 1, "category_id": 8, "bbox": [1,0,1,1], "area": 1, "iscrowd": 0}
                ]),
            )
        };
        json!({"images": [{"id": 1, "file_name": name, "width": 2, "height": 1}], "categories": categories, "annotations": annotations})
    };
    for (name, negative) in [
        ("positive.png", false),
        ("negative.png", true),
        ("unreviewed.png", true),
    ] {
        fs::write(source.join(name), PNG).unwrap();
        fs::write(
            source.join(name).with_extension("json"),
            serde_json::to_vec(&original(name, negative)).unwrap(),
        )
        .unwrap();
    }
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root).unwrap();
    let session = owner
        .create(grant.grant_id, "Export fixture".into(), 10)
        .unwrap()
        .workspace_session_id;
    let importer = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = importer
        .inspect(
            session,
            grant.grant_id,
            "Synthetic".into(),
            vec![
                "positive.png".into(),
                "negative.png".into(),
                "unreviewed.png".into(),
            ],
        )
        .unwrap();
    let registered = importer
        .confirm(session, plan.plan_id, plan.fingerprint, 11)
        .unwrap();
    let dataset = Uuid::parse_str(&registered.dataset_id).unwrap();
    let page = owner.list_assets(session, dataset, None, 4).unwrap();
    let find = |name| {
        page.items
            .iter()
            .find(|asset| asset.filename == name)
            .unwrap()
            .asset_id
    };
    let drafts = ReviewDraftService::new(Arc::clone(&owner)).unwrap();
    let finals = ReviewFinalService::new(Arc::clone(&owner)).unwrap();
    let interpreter = PathBuf::from(std::env::var_os("DD_WORKER_PYTHON").unwrap());
    let worker_entry = repository.join("services/python-worker/src/dd_worker/entrypoint.py");
    let mut worker =
        GeometryRuntime::launch(Arc::clone(&owner), session, interpreter, worker_entry)
            .await
            .unwrap();
    for (name, negative) in [("positive.png", false), ("negative.png", true)] {
        let asset = find(name);
        let outcome = worker
            .audit(GeometryAuditRequest {
                dataset_id: dataset,
                asset_id: asset,
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                max_pixels: 100,
            })
            .await
            .unwrap();
        assert_eq!(outcome.status, "SUCCEEDED");
        let review = owner.open_review(session, dataset, asset, 12).unwrap();
        let mut document = original(name, negative);
        for category in document["categories"].as_array_mut().unwrap() {
            category["id"] = if category["name"] == "helmet" {
                json!(1)
            } else {
                json!(2)
            };
        }
        for annotation in document["annotations"].as_array_mut().unwrap() {
            annotation["category_id"] = if annotation["category_id"] == 8 {
                json!(1)
            } else {
                json!(2)
            };
        }
        let draft = drafts
            .save(
                session,
                ReviewDraftRequest {
                    review_id: review.review_id,
                    source_task_id: outcome.task_id,
                    expected_review_revision: 0,
                    expected_draft_revision: None,
                    coco_document: serde_json::to_vec(&document).unwrap(),
                },
                13,
            )
            .unwrap();
        let operation = finals
            .prepare(
                session,
                ReviewFinalRequest {
                    review_id: review.review_id,
                    expected_review_revision: draft.review_revision,
                    base_image_digest: review.base_image_digest,
                    base_annotation_digest: review.base_annotation_digest,
                    draft_digest: draft.draft_digest,
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                    confirmed_negative: negative,
                },
                14,
            )
            .unwrap();
        assert_eq!(
            finals
                .resume(session, operation.operation_id, 15)
                .unwrap()
                .state,
            FinalOperationState::Committed
        );
    }
    worker.shutdown().await.unwrap();

    let listed = owner.list_assets(session, dataset, None, 4).unwrap();
    let state = |name: &str| {
        listed
            .items
            .iter()
            .find(|entry| entry.filename == name)
            .unwrap()
            .annotation_state
    };
    assert_eq!(
        state("positive.png"),
        AssetAnnotationState::ConfirmedPositive
    );
    assert_eq!(
        state("negative.png"),
        AssetAnnotationState::ConfirmedNegative
    );
    assert_eq!(state("unreviewed.png"), AssetAnnotationState::Unreviewed);

    let export = CocoExportService::new(Arc::clone(&owner)).unwrap();
    let request = |destination_grant_id, policy| CocoExportRequest {
        dataset_id: dataset,
        destination_grant_id,
        policy,
    };
    let read_grant = owner
        .grant_import_directory(session, destination.clone())
        .unwrap();
    assert!(
        export
            .export(
                session,
                request(read_grant.grant_id, CocoExportPolicy::ExcludeIneligible)
            )
            .is_err()
    );
    let grant = owner
        .grant_export_directory(session, destination.clone())
        .unwrap();
    assert!(
        export
            .export(session, request(grant.grant_id, CocoExportPolicy::Strict))
            .is_err()
    );
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    let grant = owner
        .grant_export_directory(session, destination.clone())
        .unwrap();
    let receipt = export
        .export(
            session,
            request(grant.grant_id, CocoExportPolicy::ExcludeIneligible),
        )
        .unwrap();
    assert!(
        export
            .export(
                session,
                request(grant.grant_id, CocoExportPolicy::ExcludeIneligible)
            )
            .is_err()
    );
    assert_eq!(receipt.image_count, 2);
    assert_eq!(receipt.annotation_count, 2);
    assert_eq!(receipt.exclusions.len(), 1);
    assert_eq!(receipt.exclusions[0].asset_id, find("unreviewed.png"));
    assert_eq!(receipt.exclusions[0].reason, ExclusionReason::Unconfirmed);
    let manifest: Value =
        serde_json::from_slice(&fs::read(receipt.package.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["excluded"][0]["asset_id"],
        find("unreviewed.png").to_string()
    );
    assert_eq!(manifest["images"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["annotations"].as_array().unwrap().len(), 2);
    for asset in ["positive.png", "negative.png", "unreviewed.png"] {
        assert_eq!(fs::read(source.join(asset)).unwrap(), PNG);
    }
    assert_reader(&repository, &receipt.package);

    owner
        .open_review(session, dataset, find("positive.png"), 16)
        .unwrap();
    let changed = source.join("negative.json");
    fs::write(
        &changed,
        [fs::read(&changed).unwrap(), b" ".to_vec()].concat(),
    )
    .unwrap();
    let after = owner.list_assets(session, dataset, None, 4).unwrap();
    assert!(
        after
            .items
            .iter()
            .any(|entry| entry.filename == "positive.png" && entry.active_review)
    );
    assert!(
        after
            .items
            .iter()
            .any(|entry| entry.filename == "negative.png"
                && entry.annotation_state == AssetAnnotationState::Stale)
    );
    let ineligible = export.eligibility(session, dataset).unwrap();
    assert!(
        ineligible
            .iter()
            .any(|item| item.asset_id == find("positive.png")
                && item.reason == ExclusionReason::ActiveReview)
    );
    assert!(
        ineligible
            .iter()
            .any(|item| item.asset_id == find("negative.png")
                && item.reason == ExclusionReason::InputChanged)
    );
    owner.close(session).unwrap();
}

fn assert_reader(repository: &Path, export: &Path) {
    let mut reader = Command::new(
        std::env::var_os("DD_COCO_READER_PYTHON").expect("set COCO reader interpreter"),
    );
    reader
        .arg("-I")
        .arg("-B")
        .arg(repository.join("tests/integration/rpc/read_coco_export.py"))
        .arg(export);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        reader.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let result = reader.output().unwrap();
    assert!(
        result.status.success(),
        "COCO reader: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    print!("{}", String::from_utf8_lossy(&result.stdout));
}
