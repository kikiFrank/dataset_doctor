use std::fs;
use std::path::{Path, PathBuf};

use dd_infra::import_preflight::{Annotation, ImportProblem, inspect_single_image_coco};
use dd_infra::json::JsonContracts;

const PNG: &[u8] = include_bytes!("../../../testdata/image_io/p00-worker-rgb.png");

fn root(case: &str) -> PathBuf {
    let run = PathBuf::from(std::env::var_os("DD_IMPORT_PREFLIGHT_RUN_DIR").unwrap())
        .canonicalize()
        .unwrap();
    let runs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/import-preflight/runs")
        .canonicalize()
        .unwrap();
    assert_eq!(run.parent(), Some(runs.as_path()));
    let root = run.join(case);
    fs::create_dir(&root).unwrap();
    root
}

fn labels(file_name: &str, category: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "images": [{"id": 1, "file_name": file_name, "width": 2, "height": 2}],
        "annotations": [{"id": 1, "image_id": 1, "category_id": 1, "bbox": [0, 0, 1, 1], "area": 1, "iscrowd": 0}],
        "categories": [{"id": 1, "name": category}]
    })).unwrap()
}

fn image(root: &Path, path: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, PNG).unwrap();
}

#[test]
fn detects_local_categories_and_missing_annotations_without_writing_source() {
    let root = root("合成导入 and filename spaces");
    let contracts = JsonContracts::new().unwrap();
    image(&root, "a/shared.png");
    image(&root, "b/shared.png");
    image(&root, "missing.png");
    let a = labels("shared.png", "helmet");
    let b = labels("b/shared.png", "gloves");
    fs::write(root.join("a/shared.json"), &a).unwrap();
    fs::write(root.join("b/shared.json"), &b).unwrap();

    let selected = [
        PathBuf::from("a/shared.png"),
        PathBuf::from("b/shared.png"),
        PathBuf::from("missing.png"),
    ];
    let preview = inspect_single_image_coco(&root, &contracts, &selected).unwrap();
    assert_eq!(preview.inputs.len(), 3);
    assert_eq!(
        preview.inputs[0].media_sha256,
        preview.inputs[1].media_sha256
    );
    assert_eq!(preview.inputs[0].media_sha256.as_ref().unwrap().len(), 64);
    for (input, category) in preview.inputs[..2].iter().zip(["helmet", "gloves"]) {
        let Annotation::Reference {
            categories, sha256, ..
        } = &input.annotation
        else {
            panic!("expected labels");
        };
        assert_eq!(
            (categories[0].id, categories[0].name.as_str()),
            (1, category)
        );
        assert_eq!(sha256.len(), 64);
    }
    assert_eq!(preview.inputs[2].annotation, Annotation::Missing);
    assert!(!root.join("missing.json").exists());
    assert_eq!(fs::read(root.join("a/shared.json")).unwrap(), a);
    assert_eq!(fs::read(root.join("b/shared.json")).unwrap(), b);
    assert_eq!(fs::read(root.join("missing.png")).unwrap(), PNG);
}

#[test]
fn reports_ambiguity_and_rejects_invalid_documents_instead_of_treating_them_as_empty() {
    let root = root("ambiguous sidecars");
    let contracts = JsonContracts::new().unwrap();
    for name in ["same.jpg", "same.png", "wrong.png", "unsupported.png"] {
        image(&root, name);
    }
    fs::write(root.join("same.json"), labels("same.jpg", "helmet")).unwrap();
    fs::write(root.join("wrong.json"), labels("different.png", "helmet")).unwrap();
    let mut unsupported: serde_json::Value =
        serde_json::from_slice(&labels("unsupported.png", "helmet")).unwrap();
    unsupported["annotations"][0]["segmentation"] = serde_json::json!([[0, 0, 1, 1, 1, 0]]);
    fs::write(root.join("unsupported.json"), unsupported.to_string()).unwrap();

    let preview = inspect_single_image_coco(
        &root,
        &contracts,
        &[
            "same.jpg".into(),
            "wrong.png".into(),
            "unsupported.png".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        preview.inputs[0].annotation,
        Annotation::Blocked(ImportProblem::AmbiguousSidecar)
    );
    assert_eq!(
        preview.inputs[1].annotation,
        Annotation::Blocked(ImportProblem::WrongImage)
    );
    assert!(matches!(
        preview.inputs[2].annotation,
        Annotation::Blocked(ImportProblem::UnsupportedAnnotation(_))
    ));
}

#[test]
fn reinspection_detects_changed_bytes_and_paths_outside_the_selection_are_rejected() {
    let root = root("changed imports");
    let contracts = JsonContracts::new().unwrap();
    image(&root, "a.png");
    let path = root.join("a.json");
    let selected = [PathBuf::from("a.png")];
    let missing = inspect_single_image_coco(&root, &contracts, &selected).unwrap();
    fs::write(&path, labels("a.png", "helmet")).unwrap();
    let planned = inspect_single_image_coco(&root, &contracts, &selected).unwrap();
    assert_ne!(planned, missing);
    assert_eq!(
        planned,
        inspect_single_image_coco(&root, &contracts, &selected).unwrap()
    );
    let mut contents = fs::read(&path).unwrap();
    contents.push(b'\n');
    fs::write(&path, contents).unwrap();
    assert_ne!(
        planned,
        inspect_single_image_coco(&root, &contracts, &selected).unwrap()
    );
    for invalid in [
        root.join("a.png"),
        PathBuf::from("../a.png"),
        PathBuf::from("a.png:outside.png"),
    ] {
        assert!(inspect_single_image_coco(&root, &contracts, &[invalid]).is_err());
    }
}

#[test]
fn rejects_invalid_local_references_but_preserves_geometry_for_the_worker() {
    let root = root("local coco references");
    let contracts = JsonContracts::new().unwrap();
    for name in ["bad.png", "geometry.png"] {
        image(&root, name);
    }
    let mut bad: serde_json::Value = serde_json::from_slice(&labels("bad.png", "helmet")).unwrap();
    bad["annotations"][0]["category_id"] = serde_json::json!(89);
    fs::write(root.join("bad.json"), bad.to_string()).unwrap();
    let mut geometry: serde_json::Value =
        serde_json::from_slice(&labels("geometry.png", "helmet")).unwrap();
    geometry["annotations"][0]["bbox"] = serde_json::json!([-4, 0, 7, 1]);
    fs::write(root.join("geometry.json"), geometry.to_string()).unwrap();

    let preview = inspect_single_image_coco(
        &root,
        &contracts,
        &["bad.png".into(), "geometry.png".into()],
    )
    .unwrap();
    assert!(matches!(
        preview.inputs[0].annotation,
        Annotation::Blocked(ImportProblem::UnsupportedAnnotation(_))
    ));
    assert!(matches!(
        preview.inputs[1].annotation,
        Annotation::Reference { .. }
    ));
}

#[cfg(windows)]
#[test]
fn directory_junction_does_not_authorize_a_source_outside_the_root() {
    let root = PathBuf::from(std::env::var_os("DD_IMPORT_PREFLIGHT_RUN_DIR").unwrap())
        .join("junction-source");
    assert_eq!(
        fs::read(root.join("redirected/unapproved.png")).unwrap(),
        PNG
    );
    let contracts = JsonContracts::new().unwrap();
    let preview =
        inspect_single_image_coco(&root, &contracts, &["redirected/unapproved.png".into()])
            .unwrap();
    assert!(preview.inputs[0].media_sha256.is_none());
    assert_eq!(
        preview.inputs[0].annotation,
        Annotation::Blocked(ImportProblem::ImageRead(
            std::io::ErrorKind::PermissionDenied
        ))
    );
}
