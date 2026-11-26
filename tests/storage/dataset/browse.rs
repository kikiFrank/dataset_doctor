#[path = "../workspace/support.rs"]
mod support;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use dd_core::workspace::WorkspaceAccessError;
use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::dataset_query::DatasetQueryError;
use dd_infra::workspace_thread::{WorkspaceThread, WorkspaceThreadError};
use uuid::Uuid;

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

#[test]
fn browse_pages_are_session_scoped_and_separate_dataset_members() {
    let root = support::workspace_root("browse datasets Workspace");
    let source = support::workspace_root("browse dataset source");
    for path in ["a.png", "b.png", "another.png"] {
        fs::write(source.join(path), IMAGE).unwrap();
    }
    fs::write(source.join("a.json"), br#"{"images":[{"id":1,"file_name":"a.png","width":2,"height":1}],"annotations":[],"categories":[]}"#).unwrap();
    let thread = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let selection = thread.grant_selected_directory(root).unwrap();
    let workspace = thread
        .create(selection.grant_id, "Browse".into(), 10)
        .unwrap();
    let session = workspace.workspace_session_id;
    let importer = DatasetImportCoordinator::new(Arc::clone(&thread)).unwrap();
    let import = |name: &str, images: &[&str]| {
        let selection = thread
            .grant_import_directory(session, source.clone())
            .unwrap();
        let plan = importer
            .inspect(
                session,
                selection.grant_id,
                name.into(),
                images.iter().copied().map(Into::into).collect(),
            )
            .unwrap();
        let receipt = importer
            .confirm(session, plan.plan_id, plan.fingerprint, 20)
            .unwrap();
        Uuid::parse_str(&receipt.dataset_id).unwrap()
    };
    let first_dataset = import("头盔数据集", &["b.png", "a.png"]);
    let second_dataset = import("第二个数据集", &["another.png"]);

    let first = thread.list_datasets(session, None, 1).unwrap();
    assert_eq!(first.items.len(), 1);
    let second = thread.list_datasets(session, first.next_cursor, 1).unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
    let mut datasets = [first.items[0].dataset_id, second.items[0].dataset_id];
    datasets.sort_unstable();
    let mut expected = [first_dataset, second_dataset];
    expected.sort_unstable();
    assert_eq!(datasets, expected);
    let first = first
        .items
        .iter()
        .chain(&second.items)
        .find(|item| item.dataset_id == first_dataset)
        .unwrap();
    assert_eq!(
        (
            first.name.as_str(),
            first.asset_count,
            first.unconfirmed_count
        ),
        ("头盔数据集", 2, 2)
    );

    let page = thread.list_assets(session, first_dataset, None, 1).unwrap();
    assert_eq!(page.items.len(), 1);
    let rest = thread
        .list_assets(session, first_dataset, page.next_cursor, 1)
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next_cursor.is_none());
    let mut items: Vec<_> = page.items.into_iter().chain(rest.items).collect();
    items.sort_by(|left, right| left.filename.cmp(&right.filename));
    let a = &items[0];
    let b = &items[1];
    assert_eq!(
        (a.filename.as_str(), a.has_annotation, a.final_review_id),
        ("a.png", true, None)
    );
    assert_eq!(
        (b.filename.as_str(), b.has_annotation, b.final_review_id),
        ("b.png", false, None)
    );
    assert!(matches!(
        thread.list_assets(session, Uuid::new_v4(), None, 1),
        Err(WorkspaceThreadError::Dataset(
            DatasetQueryError::UnknownDataset
        ))
    ));
    assert!(matches!(
        thread.list_datasets(session, None, 0),
        Err(WorkspaceThreadError::Dataset(
            DatasetQueryError::InvalidPageSize
        ))
    ));
    assert!(matches!(
        thread.list_datasets(Uuid::new_v4(), None, 1),
        Err(WorkspaceThreadError::Access(
            WorkspaceAccessError::StaleSession
        ))
    ));
}
