//! Core validation for the registered workflow templates.

mod adjustment;

pub use adjustment::{
    AcceptedPlan, AdjustmentAction, AdjustmentEffect, AdjustmentError, AdjustmentRecord,
    PlanAdjustment,
};

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use dd_domain::workflow_templates::{DATASET_HEALTHCHECK, REPAIR_RECHECK, VIDEO_TO_DATASET};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_SAMPLE_LIMIT: u32 = 100_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeKind {
    Dataset,
    VideoSegment,
    RepairCandidate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalGate {
    FinalAnnotation,
    StagingInjection,
    RepairApplication,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Step {
    DatasetProfile,
    AnnotationGeometry,
    ImageQuality,
    DetectorPredictions,
    VisualReview,
    VideoTracking,
    VideoSampling,
    StagingQuality,
    ColorPreview,
    Recheck,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum EnabledStep {
    DatasetProfile {
        sample_limit: u32,
    },
    AnnotationGeometry {
        sample_limit: u32,
    },
    ImageQuality {
        sample_limit: u32,
    },
    DetectorPredictions {
        sample_limit: u32,
    },
    VisualReview {
        sample_limit: u32,
        model_profile_id: Uuid,
        model_calls: u16,
        output_tokens: u64,
    },
    VideoTracking,
    VideoSampling,
    StagingQuality {
        sample_limit: u32,
    },
    ColorPreview,
    Recheck,
}

impl EnabledStep {
    pub fn step(self) -> Step {
        match self {
            Self::DatasetProfile { .. } => Step::DatasetProfile,
            Self::AnnotationGeometry { .. } => Step::AnnotationGeometry,
            Self::ImageQuality { .. } => Step::ImageQuality,
            Self::DetectorPredictions { .. } => Step::DetectorPredictions,
            Self::VisualReview { .. } => Step::VisualReview,
            Self::VideoTracking => Step::VideoTracking,
            Self::VideoSampling => Step::VideoSampling,
            Self::StagingQuality { .. } => Step::StagingQuality,
            Self::ColorPreview => Step::ColorPreview,
            Self::Recheck => Step::Recheck,
        }
    }

    fn sample_limit(self) -> Option<u32> {
        match self {
            Self::DatasetProfile { sample_limit }
            | Self::AnnotationGeometry { sample_limit }
            | Self::ImageQuality { sample_limit }
            | Self::DetectorPredictions { sample_limit }
            | Self::VisualReview { sample_limit, .. }
            | Self::StagingQuality { sample_limit } => Some(sample_limit),
            Self::VideoTracking | Self::VideoSampling | Self::ColorPreview | Self::Recheck => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Fallback {
    RetrySafeStep,
    RequestHumanReview,
    SkipWithReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplatePlan {
    pub template_id: String,
    pub template_contract_version: u16,
    pub input_manifest_id: Uuid,
    pub expected_scope_fingerprint: [u8; 32],
    pub enabled_steps: Vec<EnabledStep>,
    pub allowed_fallbacks: BTreeMap<Step, Vec<Fallback>>,
    pub max_adjustments: u8,
    pub required_review_gates: Vec<ApprovalGate>,
}

pub struct WorkflowInput {
    pub kind: ScopeKind,
    pub manifest_id: Uuid,
    pub fingerprint: [u8; 32],
}

pub struct PlanPolicy {
    pub available_steps: BTreeSet<Step>,
    pub allowed_model_profiles: BTreeSet<Uuid>,
    pub max_sample_limit: u32,
    pub max_adjustments: u8,
    pub max_model_calls: u16,
    pub max_output_tokens: u64,
}

pub struct Template {
    pub id: &'static str,
    pub version: u16,
    pub scope: ScopeKind,
    pub required_steps: &'static [Step],
    pub optional_steps: &'static [Step],
    pub gates: &'static [ApprovalGate],
}

const TEMPLATES: &[Template] = &[
    Template {
        id: DATASET_HEALTHCHECK.id,
        version: DATASET_HEALTHCHECK.version,
        scope: ScopeKind::Dataset,
        required_steps: &[Step::DatasetProfile, Step::AnnotationGeometry],
        optional_steps: &[
            Step::ImageQuality,
            Step::DetectorPredictions,
            Step::VisualReview,
        ],
        gates: &[ApprovalGate::FinalAnnotation],
    },
    Template {
        id: VIDEO_TO_DATASET.id,
        version: VIDEO_TO_DATASET.version,
        scope: ScopeKind::VideoSegment,
        required_steps: &[Step::VideoSampling, Step::StagingQuality],
        optional_steps: &[Step::VideoTracking, Step::VisualReview],
        gates: &[
            ApprovalGate::FinalAnnotation,
            ApprovalGate::StagingInjection,
        ],
    },
    Template {
        id: REPAIR_RECHECK.id,
        version: REPAIR_RECHECK.version,
        scope: ScopeKind::RepairCandidate,
        required_steps: &[Step::ColorPreview, Step::Recheck],
        optional_steps: &[Step::VisualReview],
        gates: &[
            ApprovalGate::RepairApplication,
            ApprovalGate::FinalAnnotation,
        ],
    },
];

pub fn template(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|template| template.id == id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    UnregisteredTemplate,
    TemplateVersion,
    WrongScope,
    InputChanged,
    RequiredGates,
    MissingRequiredStep(Step),
    StepNotAllowed(Step),
    DuplicateStep(Step),
    CapabilityUnavailable(Step),
    Parameters(Step),
    FallbackNotAllowed(Step),
    ModelNotAllowed,
    BudgetExceeded,
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match *self {
            Self::UnregisteredTemplate => "Workflow template is not registered",
            Self::TemplateVersion => "Workflow template contract version differs",
            Self::WrongScope => "Workflow template does not bind this scope",
            Self::InputChanged => "Workflow input does not match the authorized manifest",
            Self::RequiredGates => "Workflow plan must preserve the template approval gates",
            Self::MissingRequiredStep(step) => {
                return write!(formatter, "Required step {step:?} is missing");
            }
            Self::StepNotAllowed(step) => {
                return write!(formatter, "Step {step:?} is outside this template");
            }
            Self::DuplicateStep(step) => {
                return write!(formatter, "Step {step:?} occurs more than once");
            }
            Self::CapabilityUnavailable(step) => {
                return write!(formatter, "Step {step:?} is unavailable");
            }
            Self::Parameters(step) => {
                return write!(
                    formatter,
                    "Parameters for {step:?} are outside the allowed bounds"
                );
            }
            Self::FallbackNotAllowed(step) => {
                return write!(
                    formatter,
                    "The fallback for {step:?} is outside the template policy"
                );
            }
            Self::ModelNotAllowed => "Model profile is outside the authorized policy",
            Self::BudgetExceeded => "Workflow plan exceeds the allowed budget",
        };
        formatter.write_str(message)
    }
}

impl Error for PlanError {}

pub fn validate_plan(
    input: &WorkflowInput,
    policy: &PlanPolicy,
    plan: &TemplatePlan,
) -> Result<(), PlanError> {
    let template = template(&plan.template_id).ok_or(PlanError::UnregisteredTemplate)?;
    if plan.template_contract_version != template.version {
        return Err(PlanError::TemplateVersion);
    }
    if input.kind != template.scope {
        return Err(PlanError::WrongScope);
    }
    if input.manifest_id.is_nil()
        || input.manifest_id != plan.input_manifest_id
        || input.fingerprint != plan.expected_scope_fingerprint
    {
        return Err(PlanError::InputChanged);
    }
    if plan.required_review_gates.as_slice() != template.gates {
        return Err(PlanError::RequiredGates);
    }
    if plan.max_adjustments > policy.max_adjustments {
        return Err(PlanError::BudgetExceeded);
    }
    let mut enabled = BTreeMap::new();
    let max_samples = policy.max_sample_limit.min(MAX_SAMPLE_LIMIT);
    for &item in &plan.enabled_steps {
        let step = item.step();
        if !template.required_steps.contains(&step) && !template.optional_steps.contains(&step) {
            return Err(PlanError::StepNotAllowed(step));
        }
        if enabled.insert(step, item).is_some() {
            return Err(PlanError::DuplicateStep(step));
        }
        if !policy.available_steps.contains(&step) {
            return Err(PlanError::CapabilityUnavailable(step));
        }
        if item
            .sample_limit()
            .is_some_and(|limit| limit == 0 || limit > max_samples)
        {
            return Err(PlanError::Parameters(step));
        }
        if let EnabledStep::VisualReview {
            model_profile_id,
            model_calls,
            output_tokens,
            ..
        } = item
        {
            if model_profile_id.is_nil()
                || !policy.allowed_model_profiles.contains(&model_profile_id)
            {
                return Err(PlanError::ModelNotAllowed);
            }
            if model_calls == 0
                || output_tokens == 0
                || model_calls > policy.max_model_calls
                || output_tokens > policy.max_output_tokens
            {
                return Err(PlanError::BudgetExceeded);
            }
        }
    }
    for &step in template.required_steps {
        if !enabled.contains_key(&step) {
            return Err(PlanError::MissingRequiredStep(step));
        }
    }
    for (&step, actions) in &plan.allowed_fallbacks {
        let optional = template.optional_steps.contains(&step);
        let mut actions_seen = BTreeSet::new();
        let item = *enabled
            .get(&step)
            .ok_or(PlanError::FallbackNotAllowed(step))?;
        if actions
            .iter()
            .any(|&action| !actions_seen.insert(action) || !allows_fallback(item, optional, action))
        {
            return Err(PlanError::FallbackNotAllowed(step));
        }
    }
    Ok(())
}

fn allows_fallback(item: EnabledStep, optional: bool, action: Fallback) -> bool {
    match action {
        Fallback::RetrySafeStep => match item {
            EnabledStep::VisualReview { model_calls, .. } => model_calls >= 2,
            EnabledStep::VideoSampling => false,
            _ => true,
        },
        Fallback::RequestHumanReview => matches!(
            item.step(),
            Step::VisualReview | Step::VideoTracking | Step::Recheck
        ),
        Fallback::SkipWithReason => optional,
    }
}
