use std::fs;
use std::path::{Path, PathBuf};

use dd_infra::backup_assets::{BackupAssetError, BackupAssetRole, stage_registered_backup_assets};
use dd_infra::import_preflight::inspect_single_image_coco;
use dd_infra::json::JsonContracts;
use dd_infra::workspace::create_workspace;
use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");
const VIDEO: &[u8] = b"synthetic bytes registered as a source video fixture";

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    source: PathBuf,
    staging: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let workspace = root_path.join("workspace");
        let source = root_path.join("source");
        let staging = root_path.join("staging");
        for dir in [&workspace, &source, &staging] {
            fs::create_dir(dir).unwrap();
        }
        fs::write(source.join("sample.png"), IMAGE).unwrap();
        fs::write(source.join("unregistered.png"), IMAGE).unwrap();
        fs::write(source.join("never copy.env"), b"unregistered content").unwrap();
        let labels = serde_json::json!({
            "images": [{"id": 1, "file_name": "sample.png", "width": 2, "height": 1}],
            "annotations": [],
            "categories": [{"id": 1, "name": "synthetic"}]
        });
        fs::write(source.join("sample.json"), labels.to_string()).unwrap();
        let contracts = JsonContracts::new().unwrap();
        let preview =
            inspect_single_image_coco(&source, &contracts, &["sample.png".into()]).unwrap();
        let mut opened = create_workspace(
            &workspace,
            &contracts,
            250,
            "Backup fixtures",
            &Uuid::new_v4().to_string(),
            100,
        )
        .unwrap();
        opened
            .register_external_coco(&contracts, "Samples", &preview, 200)
            .unwrap();
        drop(opened);
        let video_id = Uuid::new_v4();
        db(&workspace)
            .execute(
                "INSERT INTO assets(asset_id, kind, location_kind, path, original_filename,
                 file_origin, media_digest, created_at_ms, updated_at_ms)
             VALUES (?1, 'VIDEO', 'WORKSPACE', 'media/source.mp4', 'source.mp4',
                 'IMPORTED', ?2, 200, 200)",
                params![video_id.to_string(), digest(VIDEO)],
            )
            .unwrap();
        fs::create_dir(workspace.join("media")).unwrap();
        fs::write(workspace.join("media/source.mp4"), VIDEO).unwrap();
        Self {
            _root: root,
            workspace,
            source,
            staging,
        }
    }
}

#[test]
fn packages_registered_image_annotation_and_workspace_video_without_neighbor_files() {
    let fixture = Fixture::new();
    let snapshot = Connection::open_with_flags(
        fixture.workspace.join("workspace.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let assets =
        stage_registered_backup_assets(&snapshot, &fixture.workspace, &fixture.staging).unwrap();
    assert_eq!(assets.len(), 3);
    for item in assets {
        assert!(item.package_path.starts_with("assets/"));
        let (expected, original) = match item.role {
            BackupAssetRole::Annotation => (
                fs::read(fixture.source.join("sample.json")).unwrap(),
                fixture.source.join("sample.json"),
            ),
            BackupAssetRole::Media if item.source_path == Path::new("media/source.mp4") => {
                (VIDEO.to_vec(), PathBuf::from("media/source.mp4"))
            }
            BackupAssetRole::Media => (IMAGE.to_vec(), fixture.source.join("sample.png")),
        };
        assert_eq!(item.source_path, original);
        assert_eq!(item.sha256, digest(&expected));
        assert_eq!(item.size_bytes, expected.len() as u64);
        assert_eq!(
            fs::read(fixture.staging.join(item.package_path)).unwrap(),
            expected
        );
    }
    let count: usize = fs::read_dir(fixture.staging.join("assets"))
        .unwrap()
        .map(|entry| fs::read_dir(entry.unwrap().path()).unwrap().count())
        .sum();
    assert_eq!(count, 3);
}

#[test]
fn stale_or_missing_registered_data_does_not_leave_a_partial_asset_tree() {
    let fixture = Fixture::new();
    let snapshot = db(&fixture.workspace);
    fs::write(fixture.source.join("sample.json"), b"changed").unwrap();
    let error = stage_registered_backup_assets(&snapshot, &fixture.workspace, &fixture.staging)
        .unwrap_err();
    assert!(matches!(
        error,
        BackupAssetError::SourceChanged {
            role: BackupAssetRole::Annotation,
            ..
        }
    ));
    assert_eq!(fs::read_dir(&fixture.staging).unwrap().count(), 0);
    assert_eq!(
        fs::read(fixture.source.join("sample.json")).unwrap(),
        b"changed"
    );
    fs::remove_file(fixture.source.join("sample.png")).unwrap();
    let error = stage_registered_backup_assets(&snapshot, &fixture.workspace, &fixture.staging)
        .unwrap_err();
    assert!(matches!(
        error,
        BackupAssetError::SourceUnavailable {
            role: BackupAssetRole::Media,
            ..
        }
    ));
    assert_eq!(fs::read_dir(&fixture.staging).unwrap().count(), 0);
}

fn db(path: &Path) -> Connection {
    Connection::open(path.join("workspace.db")).unwrap()
}

fn digest(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}
