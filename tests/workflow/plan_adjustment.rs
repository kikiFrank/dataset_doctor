use std::collections::{BTreeMap, BTreeSet};

use dd_core::workflow::{
    AcceptedPlan, AdjustmentAction, AdjustmentEffect, AdjustmentError, ApprovalGate, EnabledStep,
    Fallback, PlanAdjustment, PlanPolicy, ScopeKind, Step, TemplatePlan, WorkflowInput,
};
use uuid::Uuid;

fn inputs() -> (WorkflowInput, PlanPolicy, TemplatePlan) {
    let input = WorkflowInput {
        kind: ScopeKind::Dataset,
        manifest_id: Uuid::new_v4(),
        fingerprint: [19; 32],
    };
    let model = Uuid::new_v4();
    let policy = PlanPolicy {
        available_steps: BTreeSet::from([
            Step::DatasetProfile,
            Step::AnnotationGeometry,
            Step::VisualReview,
            Step::ImageQuality,
        ]),
        allowed_model_profiles: BTreeSet::from([model]),
        max_sample_limit: 100,
        max_adjustments: 3,
        max_model_calls: 2,
        max_output_tokens: 800,
    };
    let plan = TemplatePlan {
        template_id: "industrial_dataset_healthcheck.v1".into(),
        template_contract_version: 1,
        input_manifest_id: input.manifest_id,
        expected_scope_fingerprint: input.fingerprint,
        enabled_steps: vec![
            EnabledStep::DatasetProfile { sample_limit: 100 },
            EnabledStep::AnnotationGeometry { sample_limit: 100 },
            EnabledStep::VisualReview {
                sample_limit: 10,
                model_profile_id: model,
                model_calls: 2,
                output_tokens: 800,
            },
        ],
        max_adjustments: 2,
        required_review_gates: vec![ApprovalGate::FinalAnnotation],
        allowed_fallbacks: BTreeMap::from([(
            Step::VisualReview,
            vec![
                Fallback::RetrySafeStep,
                Fallback::RequestHumanReview,
                Fallback::SkipWithReason,
            ],
        )]),
    };
    (input, policy, plan)
}

fn adjustment(expected_revision: u64, action: AdjustmentAction) -> PlanAdjustment {
    PlanAdjustment {
        expected_revision,
        reason: "Model latency reduced the available batch".into(),
        action,
    }
}

#[test]
fn applies_bounded_changes_and_only_advances_structure_revision_when_the_plan_changes() {
    let (input, policy, plan) = inputs();
    let mut state = AcceptedPlan::accept(&input, &policy, plan).unwrap();
    let locked = BTreeSet::from([Step::DatasetProfile]);
    assert_eq!(state.plan_revision(), 1);
    let effect = state
        .adjust(
            &input,
            &policy,
            &locked,
            adjustment(
                1,
                AdjustmentAction::ReduceBatch {
                    step: Step::AnnotationGeometry,
                    sample_limit: 50,
                },
            ),
        )
        .unwrap();
    assert_eq!(effect, AdjustmentEffect::PlanChanged);
    assert_eq!((state.plan_revision(), state.adjustment_count()), (2, 1));
    assert!(
        state
            .plan()
            .enabled_steps
            .contains(&EnabledStep::AnnotationGeometry { sample_limit: 50 })
    );
    let effect = state
        .adjust(
            &input,
            &policy,
            &locked,
            adjustment(
                2,
                AdjustmentAction::RequestHumanReview {
                    step: Step::VisualReview,
                },
            ),
        )
        .unwrap();
    assert_eq!(effect, AdjustmentEffect::WaitForHuman(Step::VisualReview));
    assert_eq!((state.plan_revision(), state.adjustment_count()), (2, 2));
    assert_eq!(
        state.plan().required_review_gates,
        [ApprovalGate::FinalAnnotation]
    );
    assert_eq!(state.history()[1].resulting_plan_revision, 2);
    assert_eq!(
        state.history()[1].reason,
        "Model latency reduced the available batch"
    );
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &locked,
            adjustment(
                2,
                AdjustmentAction::RetrySafeStep {
                    step: Step::VisualReview,
                }
            )
        ),
        Err(AdjustmentError::BudgetExceeded)
    );
}

#[test]
fn requesting_a_retry_and_skipping_an_optional_step_use_distinct_effects() {
    let (input, policy, plan) = inputs();
    let mut state = AcceptedPlan::accept(&input, &policy, plan).unwrap();
    let unlocked = BTreeSet::new();
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &unlocked,
            adjustment(
                1,
                AdjustmentAction::RetrySafeStep {
                    step: Step::VisualReview,
                }
            )
        ),
        Ok(AdjustmentEffect::Retry(Step::VisualReview))
    );
    assert_eq!((state.plan_revision(), state.adjustment_count()), (1, 1));
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &unlocked,
            adjustment(
                1,
                AdjustmentAction::SkipWithReason {
                    step: Step::VisualReview,
                }
            )
        ),
        Ok(AdjustmentEffect::PlanChanged)
    );
    assert!(
        !state
            .plan()
            .enabled_steps
            .iter()
            .any(|step| step.step() == Step::VisualReview)
    );
    assert!(
        !state
            .plan()
            .allowed_fallbacks
            .contains_key(&Step::VisualReview)
    );
    assert_eq!(
        state.plan().required_review_gates,
        [ApprovalGate::FinalAnnotation]
    );
    assert_eq!((state.plan_revision(), state.adjustment_count()), (2, 2));
}

#[test]
fn a_retry_cannot_spend_the_model_call_budget_twice() {
    let (input, policy, mut plan) = inputs();
    plan.max_adjustments = 3;
    let mut state = AcceptedPlan::accept(&input, &policy, plan).unwrap();
    let unlocked = BTreeSet::new();
    let retry = AdjustmentAction::RetrySafeStep {
        step: Step::VisualReview,
    };
    assert_eq!(
        state.adjust(&input, &policy, &unlocked, adjustment(1, retry)),
        Ok(AdjustmentEffect::Retry(Step::VisualReview))
    );
    assert_eq!(
        state.adjust(&input, &policy, &unlocked, adjustment(1, retry)),
        Err(AdjustmentError::BudgetExceeded)
    );
    assert_eq!(state.adjustment_count(), 1);
}

#[test]
fn rejects_stale_or_unauthorized_changes_without_consuming_adjustment_budget() {
    let (input, policy, plan) = inputs();
    let mut state = AcceptedPlan::accept(&input, &policy, plan.clone()).unwrap();
    let locked = BTreeSet::from([Step::DatasetProfile]);
    let enable = AdjustmentAction::EnableCheck(EnabledStep::ImageQuality { sample_limit: 10 });
    assert_eq!(
        state.adjust(&input, &policy, &locked, adjustment(0, enable)),
        Err(AdjustmentError::RevisionConflict)
    );
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &locked,
            adjustment(
                1,
                AdjustmentAction::ReduceBatch {
                    step: Step::DatasetProfile,
                    sample_limit: 10,
                }
            )
        ),
        Err(AdjustmentError::StepLocked(Step::DatasetProfile))
    );
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &locked,
            adjustment(
                1,
                AdjustmentAction::SkipWithReason {
                    step: Step::AnnotationGeometry,
                }
            )
        ),
        Err(AdjustmentError::FallbackNotAllowed(
            Step::AnnotationGeometry
        ))
    );
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &locked,
            adjustment(
                1,
                AdjustmentAction::ChangeAllowedModel {
                    step: Step::VisualReview,
                    model_profile_id: Uuid::new_v4(),
                }
            )
        ),
        Err(AdjustmentError::InvalidPlan(
            dd_core::workflow::PlanError::ModelNotAllowed
        ))
    );
    assert_eq!(
        state.adjust(
            &input,
            &policy,
            &locked,
            PlanAdjustment {
                expected_revision: 1,
                reason: "  ".into(),
                action: enable,
            }
        ),
        Err(AdjustmentError::ReasonRequired)
    );
    assert_eq!((state.plan_revision(), state.adjustment_count()), (1, 0));
    assert_eq!(state.plan(), &plan);
    assert_eq!(
        state.adjust(&input, &policy, &locked, adjustment(1, enable)),
        Ok(AdjustmentEffect::PlanChanged)
    );
    assert_eq!(state.plan_revision(), 2);
}
