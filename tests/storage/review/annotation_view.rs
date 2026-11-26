#[path = "../workspace/support.rs"]
mod support;

use std::{fs, sync::Arc, time::Duration};

use dd_infra::annotation_view::AnnotationReader;
use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::dataset_query::AssetAnnotationState;
use dd_infra::workspace_thread::WorkspaceThread;
use serde_json::json;
use uuid::Uuid;

#[test]
fn reads_formal_annotation_without_inventing_final_and_detects_changed_source() {
    let root = support::workspace_root("annotation reader Workspace");
    let source = support::workspace_root("annotation reader sources");
    let png = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
    for name in ["human.png", "unlabelled.png"] {
        fs::write(source.join(name), png).unwrap();
    }
    let labels = json!({"images":[{"id": 1, "file_name":"human.png","width":2,"height":1}],
        "annotations":[], "categories":[]});
    let file = source.join("human.json");
    fs::write(&file, labels.to_string()).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root).unwrap();
    let session = owner
        .create(grant.grant_id, "Annotations".into(), 1)
        .unwrap()
        .workspace_session_id;
    let imports = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = imports
        .inspect_directory(session, grant.grant_id, "Inputs".into())
        .unwrap();
    let imported = imports
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap();
    let dataset = Uuid::parse_str(&imported.dataset_id).unwrap();
    let page = owner.list_assets(session, dataset, None, 10).unwrap();
    let id = |name: &str| {
        page.items
            .iter()
            .find(|item| item.filename == name)
            .unwrap()
            .asset_id
    };
    let reader = AnnotationReader::new(Arc::clone(&owner)).unwrap();
    let human = reader.read(session, id("human.png")).unwrap();
    assert_eq!(human.formal_coco, Some(labels.clone()));
    assert_eq!(human.annotation_state, AssetAnnotationState::Unreviewed);
    assert!(human.annotation_digest.is_some());
    assert!(human.active_review.is_none());
    let empty = reader.read(session, id("unlabelled.png")).unwrap();
    assert_eq!(empty.annotation_state, AssetAnnotationState::Unannotated);
    assert!(empty.formal_coco.is_none() && empty.annotation_digest.is_none());
    let review = owner
        .open_review(session, dataset, id("human.png"), 3)
        .unwrap();
    assert_eq!(
        reader.read(session, id("human.png")).unwrap().active_review,
        Some(review.review_id)
    );
    fs::write(&file, format!("{} ", labels)).unwrap();
    let changed = reader.read(session, id("human.png")).unwrap();
    assert_eq!(changed.formal_coco, Some(labels));
    assert_eq!(changed.annotation_state, AssetAnnotationState::Stale);
    assert_ne!(changed.annotation_digest, human.annotation_digest);
    assert!(reader.read(Uuid::new_v4(), id("human.png")).is_err());
    assert!(reader.read(session, Uuid::new_v4()).is_err());
    fs::write(&file, b"{invalid").unwrap();
    assert!(reader.read(session, id("human.png")).is_err());
    fs::write(&file, b"{\"images\":[{\"id\":1,\"file_name\":\"human.png\",\"width\":2,\"height\":1}],\"annotations\":[],\"categories\":[]}").unwrap();
    fs::write(source.join("human.png"), b"externally replaced").unwrap();
    let replaced = reader.read(session, id("human.png")).unwrap();
    assert_eq!(replaced.annotation_state, AssetAnnotationState::Stale);
    assert_ne!(replaced.image_digest, human.image_digest);
    owner.close(session).unwrap();
}
