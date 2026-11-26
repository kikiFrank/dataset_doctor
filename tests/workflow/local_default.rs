use std::collections::BTreeSet;

use dd_core::workflow::{
    ApprovalGate, DefaultSkipReason, EnabledStep, PlanError, PlanPolicy, ScopeKind, Step,
    WorkflowInput, local_default_plan, validate_plan,
};
use uuid::Uuid;

const DATASET: &str = "industrial_dataset_healthcheck.v1";
const VIDEO: &str = "video_to_dataset.v1";

#[test]
fn an_offline_healthcheck_proposes_supported_local_checks_with_explicit_coverage() {
    let input = WorkflowInput {
        kind: ScopeKind::Dataset,
        manifest_id: Uuid::new_v4(),
        fingerprint: [7; 32],
    };
    let policy = PlanPolicy {
        available_steps: BTreeSet::from([
            Step::DatasetProfile,
            Step::AnnotationGeometry,
            Step::ImageQuality,
            Step::VisualReview,
        ]),
        allowed_model_profiles: BTreeSet::new(),
        max_sample_limit: 30,
        max_adjustments: 2,
        max_model_calls: 0,
        max_output_tokens: 0,
    };
    let default = local_default_plan(DATASET, &input, &policy, 50).unwrap();
    assert_eq!(
        default.plan.enabled_steps,
        [
            EnabledStep::DatasetProfile { sample_limit: 30 },
            EnabledStep::AnnotationGeometry { sample_limit: 30 },
            EnabledStep::ImageQuality { sample_limit: 30 },
        ]
    );
    assert_eq!(
        default.plan.required_review_gates,
        [ApprovalGate::FinalAnnotation]
    );
    assert_eq!(default.plan.max_adjustments, 0);
    assert_eq!(
        default.not_executed.get(&Step::VisualReview),
        Some(&DefaultSkipReason::ModelNotConfigured)
    );
    assert_eq!(
        default.not_executed.get(&Step::DetectorPredictions),
        Some(&DefaultSkipReason::CapabilityUnavailable)
    );
    validate_plan(&input, &policy, &default.plan).unwrap();
}

#[test]
fn absent_required_algorithms_are_reported_instead_of_returning_an_executable_plan() {
    let input = WorkflowInput {
        kind: ScopeKind::VideoSegment,
        manifest_id: Uuid::new_v4(),
        fingerprint: [7; 32],
    };
    let policy = PlanPolicy {
        available_steps: BTreeSet::from([Step::StagingQuality]),
        allowed_model_profiles: BTreeSet::new(),
        max_sample_limit: 30,
        max_adjustments: 2,
        max_model_calls: 0,
        max_output_tokens: 0,
    };
    assert_eq!(
        local_default_plan(VIDEO, &input, &policy, 10).unwrap_err(),
        PlanError::CapabilityUnavailable(Step::VideoSampling)
    );
}
