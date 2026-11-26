//! Strict parsing and local JSON Schema validation at file boundaries.

mod parse;

#[cfg(feature = "execution-rpc")]
pub(crate) use parse::strict_json;

use std::error::Error;
use std::fmt::{self, Display};

use jsonschema::{Registry, Validator};
use serde_json::Value;

const BASE: &str = "https://datasetdoctor.invalid/contracts/json-schema/v1/";
const COMMON: &str = include_str!("../../../../contracts/json-schema/v1/common.schema.json");
const INPUT: &str = include_str!("../../../../contracts/json-schema/v1/input-manifest.schema.json");
const WORKSPACE: &str =
    include_str!("../../../../contracts/json-schema/v1/workspace-config.schema.json");
const COCO: &str =
    include_str!("../../../../contracts/json-schema/v1/coco-single-image.schema.json");
const TASK_RESULT: &str =
    include_str!("../../../../contracts/json-schema/v1/task-result-manifest.schema.json");
const REVIEW_DRAFT: &str =
    include_str!("../../../../contracts/json-schema/v1/review-draft.schema.json");
const TRACK_MAP: &str = include_str!("../../../../contracts/json-schema/v1/track-map.schema.json");

#[derive(Clone, Copy, Debug)]
pub enum JsonProfile {
    InputManifest,
    WorkspaceConfig,
    CocoSingleImage,
    TaskResultManifest,
    ReviewDraft,
    TrackMap,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SchemaIssue {
    pub keyword: String,
    pub instance_path: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ContractError {
    Parse(&'static str),
    Schema(Vec<SchemaIssue>),
}

impl Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(category) => write!(formatter, "JSON parse failure: {category}"),
            Self::Schema(issues) => {
                write!(formatter, "JSON violated {} schema rule(s)", issues.len())
            }
        }
    }
}

impl Error for ContractError {}

/// Compiled validators built from embedded, project-owned schemas.
pub struct JsonContracts {
    input_manifest: Validator,
    workspace_config: Validator,
    coco_single_image: Validator,
    task_result_manifest: Validator,
    review_draft: Validator,
    track_map: Validator,
}

impl JsonContracts {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let registry = Registry::new()
            .add(
                format!("{BASE}common.schema.json"),
                parse::strict_json(COMMON.as_bytes())?,
            )?
            .add(
                format!("{BASE}coco-single-image.schema.json"),
                parse::strict_json(COCO.as_bytes())?,
            )?
            .prepare()?;
        let build = |source: &str| {
            let schema = parse::strict_json(source.as_bytes())?;
            let validator = jsonschema::draft202012::options()
                .with_registry(&registry)
                .offline()
                .should_validate_formats(true)
                .build(&schema)?;
            Ok::<_, Box<dyn Error + Send + Sync>>(validator)
        };
        Ok(Self {
            input_manifest: build(INPUT)?,
            workspace_config: build(WORKSPACE)?,
            coco_single_image: build(COCO)?,
            task_result_manifest: build(TASK_RESULT)?,
            review_draft: build(REVIEW_DRAFT)?,
            track_map: build(TRACK_MAP)?,
        })
    }

    /// Validates bytes from an authorized, size-checked file and returns its JSON value.
    pub fn validate(&self, profile: JsonProfile, bytes: &[u8]) -> Result<Value, ContractError> {
        let document = parse::strict_json(bytes)?;
        let validator = match profile {
            JsonProfile::InputManifest => &self.input_manifest,
            JsonProfile::WorkspaceConfig => &self.workspace_config,
            JsonProfile::CocoSingleImage => &self.coco_single_image,
            JsonProfile::TaskResultManifest => &self.task_result_manifest,
            JsonProfile::ReviewDraft => &self.review_draft,
            JsonProfile::TrackMap => &self.track_map,
        };
        let issues: Vec<_> = validator
            .iter_errors(&document)
            .map(|error| SchemaIssue {
                keyword: error.kind().keyword().to_owned(),
                instance_path: error.instance_path().to_string(),
            })
            .collect();
        if issues.is_empty() {
            Ok(document)
        } else {
            Err(ContractError::Schema(issues))
        }
    }
}
