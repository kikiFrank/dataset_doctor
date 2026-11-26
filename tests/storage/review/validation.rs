use dd_infra::import_preflight::LocalCategory;
use dd_infra::json::JsonContracts;
use dd_infra::review_validation::{ReviewImage, ReviewValidationError, validate_review_document};
use serde_json::{Value, json};

fn fixture() -> Value {
    json!({
        "images": [{"id": 12, "file_name": "sample.png", "width": 80, "height": 60}],
        "categories": [{"id": 4, "name": "signal"}],
        "annotations": [
            {"id": 7, "image_id": 12, "category_id": 4,
             "bbox": [60.5, 45.25, 19.5, 14.75], "area": 1, "iscrowd": 0}
        ]
    })
}

fn categories() -> [LocalCategory; 2] {
    [
        LocalCategory {
            id: 4,
            name: "signal".into(),
        },
        LocalCategory {
            id: 9,
            name: "helmet".into(),
        },
    ]
}

fn image() -> ReviewImage<'static> {
    ReviewImage {
        file_name: "sample.png",
        width: 80,
        height: 60,
    }
}

#[test]
fn validates_reviewed_geometry_against_verified_image_and_registered_categories() {
    let contracts = JsonContracts::new().unwrap();
    let draft = validate_review_document(
        &contracts,
        &fixture().to_string().into_bytes(),
        image(),
        &categories(),
    )
    .unwrap();
    let final_document = draft.confirm(false).unwrap();
    assert_eq!(final_document["annotations"][0]["id"], 7);
    assert_eq!(
        final_document["annotations"][0]["bbox"],
        fixture()["annotations"][0]["bbox"]
    );
    assert_eq!(final_document["annotations"][0]["area"], 287.625);
    assert_eq!(final_document["categories"], fixture()["categories"]);
}

#[test]
fn rejects_wrong_image_duplicate_ids_unregistered_or_unlisted_categories_and_invalid_boxes() {
    let contracts = JsonContracts::new().unwrap();
    let cases = [
        (
            "/images/0/file_name",
            json!("another.png"),
            ReviewValidationError::ImageMismatch,
        ),
        (
            "/images/0/width",
            json!(81),
            ReviewValidationError::ImageMismatch,
        ),
        (
            "/annotations/0/image_id",
            json!(1),
            ReviewValidationError::AnnotationImage { annotation_id: 7 },
        ),
        (
            "/annotations/0/category_id",
            json!(9),
            ReviewValidationError::AnnotationCategory { annotation_id: 7 },
        ),
        (
            "/categories/0/name",
            json!("Signal"),
            ReviewValidationError::InvalidCategory { category_id: 4 },
        ),
        (
            "/categories/0/id",
            json!(5),
            ReviewValidationError::InvalidCategory { category_id: 5 },
        ),
        (
            "/annotations/0/bbox",
            json!([-1, 0, 10, 10]),
            ReviewValidationError::InvalidBox { annotation_id: 7 },
        ),
        (
            "/annotations/0/bbox",
            json!([0, 0, 0, 10]),
            ReviewValidationError::InvalidBox { annotation_id: 7 },
        ),
        (
            "/annotations/0/bbox",
            json!([65, 50, 16, 11]),
            ReviewValidationError::InvalidBox { annotation_id: 7 },
        ),
        (
            "/annotations/0/bbox",
            json!([80, 0, 1e-20, 1]),
            ReviewValidationError::InvalidBox { annotation_id: 7 },
        ),
    ];
    for (path, value, expected) in cases {
        let mut draft = fixture();
        *draft.pointer_mut(path).unwrap() = value;
        assert_eq!(
            validate_review_document(
                &contracts,
                draft.to_string().as_bytes(),
                image(),
                &categories()
            )
            .expect_err(path),
            expected,
            "{path}"
        );
    }
    let mut draft = fixture();
    draft["annotations"] = json!([draft["annotations"][0], draft["annotations"][0]]);
    assert_eq!(
        validate_review_document(
            &contracts,
            draft.to_string().as_bytes(),
            image(),
            &categories()
        )
        .unwrap_err(),
        ReviewValidationError::DuplicateAnnotation { annotation_id: 7 },
    );
    let malformed_number = fixture().to_string().replace("19.5", "1e999");
    assert!(
        validate_review_document(
            &contracts,
            malformed_number.as_bytes(),
            image(),
            &categories()
        )
        .is_err()
    );
}

#[test]
fn empty_draft_is_valid_but_final_requires_explicit_negative_confirmation() {
    let contracts = JsonContracts::new().unwrap();
    let mut empty = fixture();
    empty["annotations"] = json!([]);
    empty["categories"] = json!([]);
    let empty = empty.to_string();
    let check =
        |bytes: &[u8]| validate_review_document(&contracts, bytes, image(), &categories()).unwrap();
    assert_eq!(
        check(empty.as_bytes()).confirm(false).unwrap_err(),
        ReviewValidationError::NegativeConfirmationRequired
    );
    assert_eq!(
        check(empty.as_bytes()).confirm(true).unwrap()["annotations"],
        json!([])
    );
    assert_eq!(
        check(fixture().to_string().as_bytes())
            .confirm(true)
            .unwrap_err(),
        ReviewValidationError::NegativeHasBoxes
    );
}
