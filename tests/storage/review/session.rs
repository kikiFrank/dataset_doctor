#[path = "../workspace/support.rs"]
mod support;

use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::review_store::{ReviewStatus, ReviewStoreError};
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use rusqlite::Connection;
use uuid::Uuid;

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const ORIGINAL: &[u8] = br#"{"images":[{"id":1,"file_name":"sample.png","width":2,"height":1}],"annotations":[],"categories":[]}"#;

#[test]
fn reopen_resumes_one_active_review_and_cancel_preserves_formal_annotation() {
    let root = support::workspace_root("review session Workspace");
    let source = support::workspace_root("review session inputs");
    fs::write(source.join("sample.png"), IMAGE).unwrap();
    fs::write(source.join("sample.json"), ORIGINAL).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let selection = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(selection.grant_id, "Review".into(), 10)
        .unwrap()
        .workspace_session_id;
    let importer = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let selection = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = importer
        .inspect(
            session,
            selection.grant_id,
            "Samples".into(),
            vec!["sample.png".into()],
        )
        .unwrap();
    let imported = importer
        .confirm(session, plan.plan_id, plan.fingerprint, 20)
        .unwrap();
    let dataset = Uuid::parse_str(&imported.dataset_id).unwrap();
    let asset = owner.list_assets(session, dataset, None, 1).unwrap().items[0].asset_id;
    let baseline = owner.registered_image(session, dataset, asset).unwrap();
    assert!(
        owner
            .open_review(session, Uuid::new_v4(), asset, 30)
            .is_err()
    );

    let ready = Arc::new(Barrier::new(2));
    let arrivals: Vec<_> = (0..2)
        .map(|_| {
            let owner = Arc::clone(&owner);
            let ready = Arc::clone(&ready);
            thread::spawn(move || {
                ready.wait();
                owner.open_review(session, dataset, asset, 30).unwrap()
            })
        })
        .collect();
    let mut arrivals = arrivals.into_iter();
    let first = arrivals.next().unwrap().join().unwrap();
    assert_eq!(arrivals.next().unwrap().join().unwrap(), first);
    assert_eq!(first.status, ReviewStatus::Open);
    assert_eq!(first.asset_id, asset);
    assert_eq!(first.dataset_id, dataset);
    assert_eq!(first.row_revision, 0);
    assert_eq!(first.base_image_digest, baseline.image.sha256);
    assert_eq!(
        first.base_annotation_digest,
        baseline.annotation.map(|file| file.sha256)
    );
    assert_eq!(
        first.base_label_schema_digest,
        baseline.label_schema_fingerprint
    );

    owner.close(session).unwrap();
    let selection = owner.grant_selected_directory(root.clone()).unwrap();
    let reopened = owner.open(selection.grant_id).unwrap().workspace_session_id;
    assert!(matches!(
        owner.cancel_review(session, first.review_id, 0, 40),
        Err(WorkspaceThreadError::Access(_))
    ));
    assert_eq!(
        owner.open_review(reopened, dataset, asset, 41).unwrap(),
        first
    );
    assert!(matches!(
        owner.cancel_review(reopened, first.review_id, 1, 42),
        Err(WorkspaceThreadError::Review(
            ReviewStoreError::RevisionConflict
        ))
    ));
    let cancelled = owner
        .cancel_review(reopened, first.review_id, 0, 43)
        .unwrap();
    assert_eq!(cancelled.status, ReviewStatus::Cancelled);
    assert_eq!(cancelled.row_revision, 1);
    assert_eq!(
        owner.get_review(reopened, first.review_id).unwrap(),
        cancelled
    );
    let second = owner.open_review(reopened, dataset, asset, 44).unwrap();
    assert_ne!(first.review_id, second.review_id);
    assert_eq!(second.status, ReviewStatus::Open);
    assert_eq!(second.base_annotation_digest, first.base_annotation_digest);
    assert_eq!(fs::read(source.join("sample.json")).unwrap(), ORIGINAL);

    owner.close(reopened).unwrap();
    let connection = Connection::open(root.join("workspace.db")).unwrap();
    let (reviews, finals): (i64, i64) = connection.query_row(
        "SELECT (SELECT count(*) FROM review_sessions), (SELECT count(final_review_id) FROM dataset_assets)",
        [], |row| Ok((row.get(0)?, row.get(1)?))
    ).unwrap();
    assert_eq!((reviews, finals), (2, 0));
}
