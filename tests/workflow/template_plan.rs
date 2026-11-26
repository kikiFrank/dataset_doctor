use std::collections::{BTreeMap, BTreeSet};

use dd_core::workflow::{
    ApprovalGate, EnabledStep, Fallback, PlanError, PlanPolicy, ScopeKind, Step, TemplatePlan,
    WorkflowInput, template, validate_plan,
};
use uuid::Uuid;

const HEALTHCHECK: &str = "industrial_dataset_healthcheck.v1";
const VIDEO: &str = "video_to_dataset.v1";
const REPAIR: &str = "repair_recheck.v1";

fn input(kind: ScopeKind) -> WorkflowInput {
    WorkflowInput {
        kind,
        manifest_id: Uuid::new_v4(),
        fingerprint: [0x19; 32],
    }
}

fn healthcheck(input: &WorkflowInput) -> TemplatePlan {
    TemplatePlan {
        template_id: HEALTHCHECK.into(),
        template_contract_version: 1,
        input_manifest_id: input.manifest_id,
        expected_scope_fingerprint: input.fingerprint,
        enabled_steps: vec![
            EnabledStep::DatasetProfile { sample_limit: 100 },
            EnabledStep::AnnotationGeometry { sample_limit: 100 },
        ],
        allowed_fallbacks: BTreeMap::new(),
        max_adjustments: 0,
        required_review_gates: vec![ApprovalGate::FinalAnnotation],
    }
}

fn local_policy() -> PlanPolicy {
    PlanPolicy {
        available_steps: BTreeSet::from([Step::DatasetProfile, Step::AnnotationGeometry]),
        allowed_model_profiles: BTreeSet::new(),
        max_sample_limit: 100,
        max_adjustments: 1,
        max_model_calls: 0,
        max_output_tokens: 0,
    }
}

#[test]
fn registry_binds_the_approved_scopes_and_human_gates() {
    assert_eq!(template(HEALTHCHECK).unwrap().scope, ScopeKind::Dataset);
    assert_eq!(template(VIDEO).unwrap().scope, ScopeKind::VideoSegment);
    assert_eq!(template(REPAIR).unwrap().scope, ScopeKind::RepairCandidate);
    assert_eq!(
        template(HEALTHCHECK).unwrap().gates,
        &[ApprovalGate::FinalAnnotation]
    );
    assert_eq!(
        template(VIDEO).unwrap().gates,
        &[
            ApprovalGate::FinalAnnotation,
            ApprovalGate::StagingInjection
        ]
    );
    assert_eq!(
        template(REPAIR).unwrap().gates,
        &[
            ApprovalGate::RepairApplication,
            ApprovalGate::FinalAnnotation
        ]
    );
    assert!(template("unrestricted.v1").is_none());
}

#[test]
fn permits_deterministic_local_checks_without_models_and_rejects_escalation() {
    let input = input(ScopeKind::Dataset);
    let mut plan = healthcheck(&input);
    let mut policy = local_policy();
    assert!(validate_plan(&input, &policy, &plan).is_ok());

    let model = Uuid::new_v4();
    plan.enabled_steps.push(EnabledStep::VisualReview {
        sample_limit: 10,
        model_profile_id: model,
        model_calls: 2,
        output_tokens: 400,
    });
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::CapabilityUnavailable(Step::VisualReview))
    );
    policy.available_steps.insert(Step::VisualReview);
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::ModelNotAllowed)
    );
    policy.allowed_model_profiles.insert(model);
    policy.max_model_calls = 1;
    policy.max_output_tokens = 400;
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::BudgetExceeded)
    );
    policy.max_model_calls = 2;
    assert!(validate_plan(&input, &policy, &plan).is_ok());
    let unauthorized = Uuid::new_v4();
    plan.enabled_steps[2] = EnabledStep::VisualReview {
        sample_limit: 10,
        model_profile_id: unauthorized,
        model_calls: 1,
        output_tokens: 100,
    };
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::ModelNotAllowed)
    );
}

#[test]
fn rejects_stale_scope_forged_manifests_and_unregistered_template_steps() {
    let input = input(ScopeKind::Dataset);
    let policy = local_policy();
    let mut plan = healthcheck(&input);
    plan.expected_scope_fingerprint = [0xff; 32];
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::InputChanged)
    );
    plan.expected_scope_fingerprint = input.fingerprint;
    plan.input_manifest_id = Uuid::new_v4();
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::InputChanged)
    );
    plan.input_manifest_id = input.manifest_id;
    plan.template_contract_version = 2;
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::TemplateVersion)
    );
    plan.template_contract_version = 1;
    plan.enabled_steps.push(EnabledStep::VideoTracking);
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::StepNotAllowed(Step::VideoTracking))
    );
    plan.enabled_steps.pop();
    plan.template_id = VIDEO.into();
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::WrongScope)
    );
}

#[test]
fn rejects_bypassed_approval_unsupported_fallbacks_and_invalid_bounds() {
    let input = input(ScopeKind::Dataset);
    let policy = local_policy();
    let mut plan = healthcheck(&input);
    plan.required_review_gates.clear();
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::RequiredGates)
    );
    plan.required_review_gates
        .push(ApprovalGate::FinalAnnotation);
    plan.allowed_fallbacks
        .insert(Step::DatasetProfile, vec![Fallback::SkipWithReason]);
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::FallbackNotAllowed(Step::DatasetProfile))
    );
    plan.allowed_fallbacks.clear();
    plan.max_adjustments = 2;
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::BudgetExceeded)
    );
    plan.max_adjustments = 1;
    plan.enabled_steps[1] = EnabledStep::AnnotationGeometry { sample_limit: 101 };
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::Parameters(Step::AnnotationGeometry))
    );
    plan.enabled_steps[1] = EnabledStep::AnnotationGeometry { sample_limit: 100 };
    plan.enabled_steps
        .push(EnabledStep::AnnotationGeometry { sample_limit: 1 });
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::DuplicateStep(Step::AnnotationGeometry))
    );
    plan.enabled_steps.truncate(1);
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::MissingRequiredStep(Step::AnnotationGeometry))
    );
}

#[test]
fn video_sampling_keeps_human_gates_without_a_tracking_result() {
    let input = input(ScopeKind::VideoSegment);
    let mut policy = local_policy();
    policy.available_steps = BTreeSet::from([Step::VideoSampling, Step::StagingQuality]);
    let plan = TemplatePlan {
        template_id: VIDEO.into(),
        template_contract_version: 1,
        input_manifest_id: input.manifest_id,
        expected_scope_fingerprint: input.fingerprint,
        enabled_steps: vec![
            EnabledStep::VideoSampling,
            EnabledStep::StagingQuality { sample_limit: 10 },
        ],
        allowed_fallbacks: BTreeMap::new(),
        max_adjustments: 0,
        required_review_gates: vec![
            ApprovalGate::FinalAnnotation,
            ApprovalGate::StagingInjection,
        ],
    };
    assert_eq!(validate_plan(&input, &policy, &plan), Ok(()));
}

#[test]
fn a_model_retry_must_be_covered_by_its_call_budget() {
    let input = input(ScopeKind::Dataset);
    let mut plan = healthcheck(&input);
    let mut policy = local_policy();
    let model = Uuid::new_v4();
    policy.available_steps.insert(Step::VisualReview);
    policy.allowed_model_profiles.insert(model);
    policy.max_model_calls = 2;
    policy.max_output_tokens = 400;
    let visual = |model_calls| EnabledStep::VisualReview {
        sample_limit: 10,
        model_profile_id: model,
        model_calls,
        output_tokens: 200,
    };
    plan.enabled_steps.push(visual(1));
    plan.allowed_fallbacks
        .insert(Step::VisualReview, vec![Fallback::RetrySafeStep]);
    assert_eq!(
        validate_plan(&input, &policy, &plan),
        Err(PlanError::FallbackNotAllowed(Step::VisualReview))
    );
    plan.enabled_steps[2] = visual(2);
    assert_eq!(validate_plan(&input, &policy, &plan), Ok(()));
}
