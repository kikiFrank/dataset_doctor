use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    EnabledStep, MAX_SAMPLE_LIMIT, PlanError, PlanPolicy, Step, TemplatePlan, WorkflowInput,
    template, validate_plan,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DefaultSkipReason {
    CapabilityUnavailable,
    ModelNotConfigured,
    NotSelectedByDefault,
}

#[derive(Debug)]
pub struct DefaultPlan {
    pub plan: TemplatePlan,
    pub not_executed: BTreeMap<Step, DefaultSkipReason>,
}

pub fn local_default_plan(
    template_id: &str,
    input: &WorkflowInput,
    policy: &PlanPolicy,
    requested_sample_limit: u32,
) -> Result<DefaultPlan, PlanError> {
    let template = template(template_id).ok_or(PlanError::UnregisteredTemplate)?;
    let sample_limit = requested_sample_limit
        .min(policy.max_sample_limit)
        .min(MAX_SAMPLE_LIMIT);
    let mut enabled_steps = Vec::new();
    for &step in template.required_steps {
        enabled_steps.push(match step {
            Step::DatasetProfile => EnabledStep::DatasetProfile { sample_limit },
            Step::AnnotationGeometry => EnabledStep::AnnotationGeometry { sample_limit },
            Step::VideoSampling => EnabledStep::VideoSampling,
            Step::StagingQuality => EnabledStep::StagingQuality { sample_limit },
            Step::ColorPreview => EnabledStep::ColorPreview,
            Step::Recheck => EnabledStep::Recheck,
            _ => return Err(PlanError::CapabilityUnavailable(step)),
        });
    }
    let mut not_executed = BTreeMap::new();
    for &step in template.optional_steps {
        if step == Step::ImageQuality && policy.available_steps.contains(&step) {
            enabled_steps.push(EnabledStep::ImageQuality { sample_limit });
            continue;
        }
        let reason = if step == Step::VisualReview && policy.allowed_model_profiles.is_empty() {
            DefaultSkipReason::ModelNotConfigured
        } else if !policy.available_steps.contains(&step) {
            DefaultSkipReason::CapabilityUnavailable
        } else {
            DefaultSkipReason::NotSelectedByDefault
        };
        not_executed.insert(step, reason);
    }
    let plan = TemplatePlan {
        template_id: template.id.into(),
        template_contract_version: template.version,
        input_manifest_id: input.manifest_id,
        expected_scope_fingerprint: input.fingerprint,
        enabled_steps,
        allowed_fallbacks: BTreeMap::new(),
        max_adjustments: 0,
        required_review_gates: template.gates.to_vec(),
    };
    validate_plan(input, policy, &plan)?;
    Ok(DefaultPlan { plan, not_executed })
}
