use std::fs;
use std::path::PathBuf;

use dd_infra::import_preflight::inspect_single_image_coco;
use dd_infra::import_registration::ImportRegistrationError;
use dd_infra::json::JsonContracts;
use dd_infra::workspace::create_workspace;
use rusqlite::Connection;

const ID: &str = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9";
const IMAGE: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

fn fixture(case: &str) -> (PathBuf, PathBuf) {
    let run = PathBuf::from(std::env::var_os("DD_IMPORT_PREFLIGHT_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(
        run.parent(),
        Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/import-preflight/runs")
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    let source = run.join(case).join("source");
    let workspace = run.join(case).join("workspace");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir(&workspace).unwrap();
    for name in ["a.png", "b.png", "c.png"] {
        fs::write(source.join(name), IMAGE).unwrap();
    }
    for (stem, category) in [("a", "helmet"), ("b", "gloves")] {
        let labels = serde_json::json!({
            "images": [{"id": 1, "file_name": format!("{stem}.png"), "width": 2, "height": 2}],
            "annotations": [{"id": 1, "image_id": 1, "category_id": 3, "bbox": [0, 0, 1, 1], "area": 1, "iscrowd": 0}],
            "categories": [{"id": 3, "name": category}]
        });
        fs::write(source.join(format!("{stem}.json")), labels.to_string()).unwrap();
    }
    (source, workspace)
}

#[test]
fn registers_original_files_deterministically_and_leaves_every_member_unreviewed() {
    let (source, workspace) = fixture("register original images");
    let contracts = JsonContracts::new().unwrap();
    let before = fs::read(source.join("a.json")).unwrap();
    let selected = ["b.png".into(), "c.png".into(), "a.png".into()];
    let preview = inspect_single_image_coco(&source, &contracts, &selected).unwrap();
    let mut opened = create_workspace(&workspace, &contracts, 250, "Import", ID, 0).unwrap();
    let receipt = opened
        .register_external_coco(&contracts, "Proof", &preview, 1234)
        .unwrap();
    assert_eq!(receipt.asset_count, 3);
    drop(opened);

    let db = Connection::open(workspace.join("workspace.db")).unwrap();
    let category_json: String = db
        .query_row(
            "SELECT categories_json FROM datasets WHERE dataset_id=?1",
            [&receipt.dataset_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&category_json).unwrap(),
        serde_json::json!([
            {"id": 1, "name": "gloves"}, {"id": 2, "name": "helmet"}
        ])
    );
    let mut members = db.prepare("SELECT a.original_filename, a.path, a.annotation_origin, a.annotation_local_image_id, d.final_review_id FROM assets a JOIN dataset_assets d USING (asset_id) ORDER BY a.original_filename").unwrap();
    let rows = members
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    for (index, (name, path, origin, local_id, final_id)) in rows.into_iter().enumerate() {
        assert_eq!(PathBuf::from(path), source.join(name));
        assert!(final_id.is_none());
        assert_eq!(
            (origin.as_str(), local_id),
            if index == 2 {
                ("NONE", None)
            } else {
                ("PREEXISTING", Some(1))
            }
        );
    }
    assert_eq!(fs::read(source.join("a.json")).unwrap(), before);
    assert!(!source.join("c.json").exists());
}

#[test]
fn rejects_stale_sources_and_rolls_back_a_partial_membership_conflict() {
    let (source, workspace) = fixture("registration conflicts");
    let contracts = JsonContracts::new().unwrap();
    let selection = ["a.png".into()];
    let original = inspect_single_image_coco(&source, &contracts, &selection).unwrap();
    let mut opened = create_workspace(&workspace, &contracts, 250, "Import", ID, 0).unwrap();
    let mut edited = fs::read(source.join("a.json")).unwrap();
    edited.push(b'\n');
    fs::write(source.join("a.json"), edited).unwrap();
    assert!(matches!(
        opened.register_external_coco(&contracts, "Stale", &original, 1000),
        Err(ImportRegistrationError::InputChanged)
    ));
    let from_current = inspect_single_image_coco(&source, &contracts, &selection).unwrap();
    opened
        .register_external_coco(&contracts, "First", &from_current, 1001)
        .unwrap();
    let conflict =
        inspect_single_image_coco(&source, &contracts, &["c.png".into(), "a.png".into()]).unwrap();
    assert!(
        opened
            .register_external_coco(&contracts, "Conflict", &conflict, 1002)
            .is_err()
    );
    drop(opened);

    let db = Connection::open(workspace.join("workspace.db")).unwrap();
    for table in ["datasets", "assets", "dataset_assets"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "partial state written into {table}");
    }
    let violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}
