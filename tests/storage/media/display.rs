#[path = "../workspace/support.rs"]
mod support;

use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

use dd_infra::dataset_import::DatasetImportCoordinator;
use dd_infra::media_display::{MediaDisplayError, MediaDisplayService};
use dd_infra::workspace_thread::WorkspaceThread;
use uuid::Uuid;

#[test]
fn media_locator_serves_only_its_registered_bytes_and_expires_with_the_workspace_session() {
    let root = support::workspace_root("media display Workspace");
    let source = support::workspace_root("media display inputs");
    let original = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
    fs::write(source.join("sample.png"), original).unwrap();
    let owner = Arc::new(WorkspaceThread::start(250, Duration::from_secs(30)).unwrap());
    let grant = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(grant.grant_id, "Display".into(), 1)
        .unwrap()
        .workspace_session_id;
    let importer = DatasetImportCoordinator::new(Arc::clone(&owner)).unwrap();
    let grant = owner
        .grant_import_directory(session, source.clone())
        .unwrap();
    let plan = importer
        .inspect_directory(session, grant.grant_id, "Images".into())
        .unwrap();
    let receipt = importer
        .confirm(session, plan.plan_id, plan.fingerprint, 2)
        .unwrap();
    let dataset = Uuid::parse_str(&receipt.dataset_id).unwrap();
    let asset = owner.list_assets(session, dataset, None, 1).unwrap().items[0].asset_id;
    let display = MediaDisplayService::new(Arc::clone(&owner), Duration::from_secs(30));
    let now = Instant::now();
    assert!(display.issue(session, Uuid::new_v4(), now).is_err());
    let locator = display.issue(session, asset, now).unwrap();
    let image = display.resolve(locator.grant_id, now).unwrap();
    assert_eq!(image.bytes, original);
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.digest, locator.resource_digest);
    assert!(display.resolve(Uuid::new_v4(), now).is_err());
    assert!(matches!(
        display.resolve(locator.grant_id, now + Duration::from_secs(30)),
        Err(MediaDisplayError::Expired)
    ));
    let locator = display.issue(session, asset, now).unwrap();
    fs::write(source.join("sample.png"), b"changed").unwrap();
    assert!(matches!(
        display.resolve(locator.grant_id, now),
        Err(MediaDisplayError::InputChanged)
    ));
    fs::write(source.join("sample.png"), original).unwrap();
    owner.close(session).unwrap();
    let grant = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(grant.grant_id).unwrap().workspace_session_id;
    assert!(display.resolve(locator.grant_id, now).is_err());
    assert!(display.issue(session, asset, now).is_err());
    let fresh = display.issue(reopened, asset, now).unwrap();
    assert_eq!(
        display.resolve(fresh.grant_id, now).unwrap().bytes,
        original
    );
    owner.close(reopened).unwrap();
}
