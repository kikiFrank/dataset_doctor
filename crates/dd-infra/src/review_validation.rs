//! Domain validation for editable COCO and explicitly confirmed Final documents.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{self, Display};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::import_preflight::LocalCategory;
use crate::json::{ContractError, JsonContracts, JsonProfile};

/// Identity and dimensions from the currently verified image.
pub struct ReviewImage<'a> {
    pub file_name: &'a str,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ReviewValidationError {
    Contract(ContractError),
    Representation,
    ImageMismatch,
    InvalidCategory { category_id: u64 },
    DuplicateAnnotation { annotation_id: u64 },
    AnnotationImage { annotation_id: u64 },
    AnnotationCategory { annotation_id: u64 },
    InvalidBox { annotation_id: u64 },
    NegativeConfirmationRequired,
    NegativeHasBoxes,
}

impl Display for ReviewValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => Display::fmt(error, formatter),
            Self::Representation => {
                formatter.write_str("COCO values exceed the supported numeric representation")
            }
            Self::ImageMismatch => formatter
                .write_str("COCO image does not match the verified filename and dimensions"),
            Self::InvalidCategory { category_id } => write!(
                formatter,
                "Duplicate or unregistered category {category_id}"
            ),
            Self::DuplicateAnnotation { annotation_id } => {
                write!(formatter, "Duplicate annotation ID {annotation_id}")
            }
            Self::AnnotationImage { annotation_id } => write!(
                formatter,
                "Annotation {annotation_id} refers to a different image"
            ),
            Self::AnnotationCategory { annotation_id } => write!(
                formatter,
                "Annotation {annotation_id} refers to a category missing from the document"
            ),
            Self::InvalidBox { annotation_id } => write!(
                formatter,
                "Annotation {annotation_id} has invalid or out-of-bounds box coordinates"
            ),
            Self::NegativeConfirmationRequired => {
                formatter.write_str("The reviewer must explicitly confirm an empty sample")
            }
            Self::NegativeHasBoxes => {
                formatter.write_str("A confirmed negative sample must have no annotation boxes")
            }
        }
    }
}

impl Error for ReviewValidationError {}

#[derive(Debug)]
pub struct ReviewDocument {
    coco: Value,
    has_boxes: bool,
}

impl ReviewDocument {
    #[cfg(feature = "execution-rpc")]
    pub(crate) fn into_coco(self) -> Value {
        self.coco
    }

    pub fn confirm(self, confirmed_negative: bool) -> Result<Value, ReviewValidationError> {
        confirm_negative(self.has_boxes, confirmed_negative)?;
        Ok(self.coco)
    }
}

pub(crate) fn confirm_negative(
    has_boxes: bool,
    confirmed_negative: bool,
) -> Result<(), ReviewValidationError> {
    match (has_boxes, confirmed_negative) {
        (false, false) => Err(ReviewValidationError::NegativeConfirmationRequired),
        (true, true) => Err(ReviewValidationError::NegativeHasBoxes),
        _ => Ok(()),
    }
}

#[derive(Deserialize)]
struct ImageIdentity {
    id: u64,
    file_name: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct BoxReference {
    id: u64,
    image_id: u64,
    category_id: u64,
    bbox: [f64; 4],
}

pub fn validate_review_document(
    contracts: &JsonContracts,
    bytes: &[u8],
    image: ReviewImage<'_>,
    dataset_categories: &[LocalCategory],
) -> Result<ReviewDocument, ReviewValidationError> {
    let mut coco = contracts
        .validate(JsonProfile::CocoSingleImage, bytes)
        .map_err(ReviewValidationError::Contract)?;
    let actual: ImageIdentity = serde_json::from_value(coco["images"][0].clone())
        .map_err(|_| ReviewValidationError::Representation)?;
    if (actual.file_name.as_str(), actual.width, actual.height)
        != (image.file_name, image.width, image.height)
    {
        return Err(ReviewValidationError::ImageMismatch);
    }
    let registered: HashMap<_, _> = dataset_categories
        .iter()
        .map(|category| (category.id, category.name.as_str()))
        .collect();
    let categories: Vec<LocalCategory> = serde_json::from_value(coco["categories"].clone())
        .map_err(|_| ReviewValidationError::Representation)?;
    let mut category_ids = HashSet::with_capacity(categories.len());
    for category in categories {
        if registered.get(&category.id).copied() != Some(category.name.as_str())
            || !category_ids.insert(category.id)
        {
            return Err(ReviewValidationError::InvalidCategory {
                category_id: category.id,
            });
        }
    }
    let annotations = coco["annotations"]
        .as_array_mut()
        .ok_or(ReviewValidationError::Representation)?;
    let mut ids = HashSet::with_capacity(annotations.len());
    for annotation in annotations.iter_mut() {
        let fields: BoxReference = serde_json::from_value(annotation.clone())
            .map_err(|_| ReviewValidationError::Representation)?;
        let annotation_id = fields.id;
        if !ids.insert(annotation_id) {
            return Err(ReviewValidationError::DuplicateAnnotation { annotation_id });
        }
        if fields.image_id != actual.id {
            return Err(ReviewValidationError::AnnotationImage { annotation_id });
        }
        if !category_ids.contains(&fields.category_id) {
            return Err(ReviewValidationError::AnnotationCategory { annotation_id });
        }
        let [x, y, width, height] = fields.bbox;
        if !fields.bbox.iter().all(|value| value.is_finite())
            || x < 0.0
            || y < 0.0
            || width <= 0.0
            || height <= 0.0
            || width > f64::from(image.width) - x
            || height > f64::from(image.height) - y
        {
            return Err(ReviewValidationError::InvalidBox { annotation_id });
        }
        annotation["area"] = json!(width * height);
    }
    let has_boxes = !annotations.is_empty();
    Ok(ReviewDocument { coco, has_boxes })
}
