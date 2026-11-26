#[path = "../workspace/support.rs"]
mod support;

use std::{fs, time::Duration};

use dd_infra::staging::{StagingQuery, StagingState};
use dd_infra::workspace_thread::WorkspaceThread;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const LABEL: &[u8] = br#"{"images":[{"id":1,"file_name":"labelled.png","width":2,"height":1}],"annotations":[],"categories":[]}"#;

#[test]
fn list_staging_derives_state_from_file_proofs_and_pages_only_the_target_dataset() {
    let root = support::workspace_root("staging list Workspace");
    let owner = WorkspaceThread::start(250, Duration::from_secs(30)).unwrap();
    let folder = owner.grant_selected_directory(root.clone()).unwrap();
    let session = owner
        .create(folder.grant_id, "Staging browser".into(), 1)
        .unwrap()
        .workspace_session_id;
    let connection = Connection::open(root.join("workspace.db")).unwrap();
    let target = Uuid::new_v4();
    let other = Uuid::new_v4();
    for dataset in [target, other] {
        connection.execute("INSERT INTO datasets(dataset_id,name,created_at_ms,updated_at_ms) VALUES(?1,'Fixture',1,1)",
            [dataset.to_string()]).unwrap();
    }
    let media_digest = format!("{:x}", Sha256::digest(IMAGE));
    let annotation_digest = format!("{:x}", Sha256::digest(LABEL));
    let assets = [
        (10, "plain.png", true, false, target),
        (11, "labelled.png", true, true, target),
        (12, "missing.png", false, false, target),
        (13, "other.png", true, false, other),
    ];
    for (number, name, present, labelled, dataset) in assets {
        let id = Uuid::from_u128(number).to_string();
        if present {
            fs::write(root.join(name), IMAGE).unwrap();
        }
        if labelled {
            fs::write(root.join("labelled.json"), LABEL).unwrap();
        }
        connection.execute(
            "INSERT INTO assets(asset_id,kind,location_kind,path,original_filename,file_origin,media_digest,
                annotation_path,annotation_format,annotation_origin,annotation_digest,created_at_ms,updated_at_ms)
             VALUES(?1,'IMAGE','WORKSPACE',?2,?2,'GENERATED',?3,?4,?5,?6,?7,1,1)",
            params![id,name,media_digest,labelled.then_some("labelled.json"),
                labelled.then_some("COCO_IMAGE_BBOX"),if labelled {"APP_CREATED"} else {"NONE"},
                labelled.then_some(annotation_digest.as_str())]).unwrap();
        connection.execute("INSERT INTO staging_items(asset_id,target_dataset_id,created_at_ms) VALUES(?1,?2,1)",
            params![id,dataset.to_string()]).unwrap();
    }
    let query = StagingQuery {
        target_dataset_id: Some(target),
        after: None,
        status: None,
        page_size: 2,
    };
    let page = owner.list_staging(session, query).unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|asset| asset.state)
            .collect::<Vec<_>>(),
        [StagingState::NeedsAnnotation, StagingState::NeedsReview]
    );
    let rest = owner
        .list_staging(
            session,
            StagingQuery {
                after: page.next_cursor,
                ..query
            },
        )
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.items[0].asset_id, Uuid::from_u128(12));
    assert_eq!(rest.items[0].state, StagingState::Offline);
    assert!(rest.next_cursor.is_none());
    let filtered = owner
        .list_staging(
            session,
            StagingQuery {
                status: Some(StagingState::NeedsReview),
                page_size: 1,
                ..query
            },
        )
        .unwrap();
    assert_eq!(filtered.items[0].asset_id, Uuid::from_u128(11));
    assert!(owner.list_staging(Uuid::new_v4(), query).is_err());
    assert!(
        owner
            .list_staging(
                session,
                StagingQuery {
                    target_dataset_id: Some(Uuid::new_v4()),
                    ..query
                }
            )
            .is_err()
    );
    assert!(
        owner
            .list_staging(
                session,
                StagingQuery {
                    page_size: 0,
                    ..query
                }
            )
            .is_err()
    );
    assert_eq!(
        owner
            .list_assets(session, target, None, 10)
            .unwrap()
            .items
            .len(),
        0
    );
    let labelled = Uuid::from_u128(11).to_string();
    let review = Uuid::new_v4().to_string();
    let schema = format!("{:x}", Sha256::digest(b"[]"));
    connection.execute(
        "INSERT INTO review_sessions(review_id,asset_id,dataset_id,status,baseline_media_digest,
            baseline_annotation_digest,baseline_label_schema_digest,confirmed_media_digest,
            confirmed_annotation_digest,confirmed_label_schema_digest,created_at_ms,updated_at_ms,confirmed_at_ms)
         VALUES(?1,?2,?3,'CONFIRMED',?4,?5,?6,?4,?5,?6,1,1,1)",
        params![review,labelled,target.to_string(),media_digest,annotation_digest,schema]).unwrap();
    connection
        .execute(
            "UPDATE staging_items SET review_id=?1 WHERE asset_id=?2",
            params![review, labelled],
        )
        .unwrap();
    let finalized = owner.list_staging(session, query).unwrap();
    assert_eq!(finalized.items[1].state, StagingState::NeedsCheck);
    fs::write(root.join("labelled.json"), b"changed annotation").unwrap();
    let changed = owner.list_staging(session, query).unwrap();
    assert_eq!(changed.items[1].state, StagingState::Conflict);
    owner.close(session).unwrap();
    let folder = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(folder.grant_id).unwrap().workspace_session_id;
    assert_eq!(
        owner.list_staging(reopened, query).unwrap().items[1].state,
        StagingState::Conflict
    );
}
