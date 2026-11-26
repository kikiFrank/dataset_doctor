//! Read-only preflight for existing per-image COCO files.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::json::{JsonContracts, JsonProfile};
use crate::paths::{is_portable_relative_file, open_scoped_file};

const MAX_SELECTION: usize = 1000;
const MAX_ANNOTATION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub struct ImportPreview {
    pub(crate) root: PathBuf,
    pub inputs: Vec<ImportInput>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ImportInput {
    pub image: PathBuf,
    pub media_sha256: Option<String>,
    pub annotation: Annotation,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Annotation {
    Missing,
    Reference {
        path: PathBuf,
        sha256: String,
        local_image_id: u64,
        categories: Vec<LocalCategory>,
    },
    Blocked(ImportProblem),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ImportProblem {
    ImageRead(ErrorKind),
    AnnotationRead(ErrorKind),
    AmbiguousSidecar,
    WrongImage,
    AnnotationTooLarge,
    UnsupportedAnnotation(String),
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct LocalCategory {
    pub id: u64,
    pub name: String,
}

#[derive(Deserialize)]
struct ImageReference {
    id: u64,
    file_name: String,
}

#[derive(Deserialize)]
struct AnnotationReference {
    id: u64,
    image_id: u64,
    category_id: u64,
}

#[derive(Deserialize)]
struct CocoReferences {
    images: [ImageReference; 1],
    annotations: Vec<AnnotationReference>,
    categories: Vec<LocalCategory>,
}

/// The caller resolves the root from a trusted grant. Relative selections are
/// bounded and validated before reading any source files.
pub fn inspect_single_image_coco(
    approved_root: &Path,
    contracts: &JsonContracts,
    selected: &[PathBuf],
) -> io::Result<ImportPreview> {
    if selected.is_empty() || selected.len() > MAX_SELECTION {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "Select 1 to 1000 images",
        ));
    }
    let mut unique = BTreeSet::new();
    for path in selected {
        if !is_portable_relative_file(path) || !is_supported_image(path) || !unique.insert(path) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "Invalid or duplicate image selection",
            ));
        }
    }
    let root = approved_root.canonicalize()?;
    if !root.is_dir() {
        return Err(io::Error::new(
            ErrorKind::NotADirectory,
            "Expected the approved import directory",
        ));
    }

    let mut sibling_counts = HashMap::new();
    let mut inputs = Vec::with_capacity(selected.len());
    for image in selected {
        let media = match open_scoped_file(&root, image).and_then(media_digest) {
            Ok(digest) => digest,
            Err(error) => {
                inputs.push(ImportInput {
                    image: image.clone(),
                    media_sha256: None,
                    annotation: Annotation::Blocked(ImportProblem::ImageRead(error.kind())),
                });
                continue;
            }
        };
        let annotation = inspect_annotation(&root, image, contracts, &mut sibling_counts)
            .unwrap_or_else(Annotation::Blocked);
        inputs.push(ImportInput {
            image: image.clone(),
            media_sha256: Some(media),
            annotation,
        });
    }
    Ok(ImportPreview { root, inputs })
}

type SiblingCounts = HashMap<PathBuf, io::Result<HashMap<String, usize>>>;

fn inspect_annotation(
    root: &Path,
    image: &Path,
    contracts: &JsonContracts,
    siblings: &mut SiblingCounts,
) -> Result<Annotation, ImportProblem> {
    let simple = image.with_extension("json");
    let mut extra_name = image
        .file_name()
        .expect("validated source file name")
        .to_os_string();
    extra_name.push(".json");
    let qualified = image.with_file_name(extra_name);
    let simple_exists = source_exists(root, &simple)?;
    let qualified_exists = source_exists(root, &qualified)?;
    let path = match (simple_exists, qualified_exists) {
        (true, true) => return Err(ImportProblem::AmbiguousSidecar),
        (false, false) => return Ok(Annotation::Missing),
        (false, true) => qualified,
        (true, false) => {
            let directory = image
                .parent()
                .expect("validated relative file has a parent");
            let stems = siblings
                .entry(directory.to_owned())
                .or_insert_with(|| count_image_stems(&root.join(directory)))
                .as_ref()
                .map_err(|error| ImportProblem::AnnotationRead(error.kind()))?;
            if stems.get(&stem_key(image)).copied().unwrap_or_default() > 1 {
                return Err(ImportProblem::AmbiguousSidecar);
            }
            simple
        }
    };

    let mut bytes = Vec::new();
    open_scoped_file(root, &path)
        .and_then(|file| file.take(MAX_ANNOTATION_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| ImportProblem::AnnotationRead(error.kind()))?;
    if bytes.len() as u64 > MAX_ANNOTATION_BYTES {
        return Err(ImportProblem::AnnotationTooLarge);
    }
    let document = contracts
        .validate(JsonProfile::CocoSingleImage, &bytes)
        .map_err(|error| ImportProblem::UnsupportedAnnotation(error.to_string()))?;
    let references: CocoReferences = serde_json::from_value(document)
        .map_err(|error| ImportProblem::UnsupportedAnnotation(error.to_string()))?;
    let named = Path::new(&references.images[0].file_name);
    if !is_portable_relative_file(named)
        || (named != image && named.as_os_str() != image.file_name().unwrap())
    {
        return Err(ImportProblem::WrongImage);
    }
    references.validate_local_ids()?;
    Ok(Annotation::Reference {
        path,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        local_image_id: references.images[0].id,
        categories: references.categories,
    })
}

impl CocoReferences {
    fn validate_local_ids(&self) -> Result<(), ImportProblem> {
        let mut categories = BTreeSet::new();
        let mut names = BTreeSet::new();
        for category in &self.categories {
            if !categories.insert(category.id) || !names.insert(&category.name) {
                return Err(ImportProblem::UnsupportedAnnotation(
                    "Duplicate category ID or name".into(),
                ));
            }
        }
        let mut annotations = BTreeSet::new();
        for annotation in &self.annotations {
            if !annotations.insert(annotation.id) {
                return Err(ImportProblem::UnsupportedAnnotation(
                    "Duplicate annotation ID".into(),
                ));
            }
            if annotation.image_id != self.images[0].id
                || !categories.contains(&annotation.category_id)
            {
                return Err(ImportProblem::UnsupportedAnnotation(
                    "Annotation references an unknown image or category".into(),
                ));
            }
        }
        Ok(())
    }
}

fn source_exists(root: &Path, relative: &Path) -> Result<bool, ImportProblem> {
    match fs::symlink_metadata(root.join(relative)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ImportProblem::AnnotationRead(error.kind())),
    }
}

fn media_digest(mut file: File) -> io::Result<String> {
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn count_image_stems(directory: &Path) -> io::Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if is_supported_image(&entry.path()) && !entry.file_type()?.is_dir() {
            *counts
                .entry(stem_key(Path::new(&entry.file_name())))
                .or_insert(0) += 1;
        }
    }
    Ok(counts)
}

fn stem_key(path: &Path) -> String {
    let stem = path
        .file_stem()
        .expect("known image format")
        .to_string_lossy();
    if cfg!(windows) {
        stem.to_uppercase()
    } else {
        stem.into_owned()
    }
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg"]
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}
