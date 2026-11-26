use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use uuid::Uuid;

use super::{
    EnabledStep, Fallback, PlanError, PlanPolicy, Step, TemplatePlan, WorkflowInput, validate_plan,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdjustmentAction {
    EnableCheck(EnabledStep),
    ChangeAllowedModel { step: Step, model_profile_id: Uuid },
    ReduceBatch { step: Step, sample_limit: u32 },
    RetrySafeStep { step: Step },
    RequestHumanReview { step: Step },
    SkipWithReason { step: Step },
}

pub struct PlanAdjustment {
    pub expected_revision: u64,
    pub reason: String,
    pub action: AdjustmentAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdjustmentEffect {
    PlanChanged,
    Retry(Step),
    WaitForHuman(Step),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdjustmentRecord {
    pub resulting_plan_revision: u64,
    pub reason: String,
    pub action: AdjustmentAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdjustmentError {
    InvalidPlan(PlanError),
    RevisionConflict,
    BudgetExceeded,
    ReasonRequired,
    InvalidChoice(Step),
    StepLocked(Step),
    FallbackNotAllowed(Step),
}

impl fmt::Display for AdjustmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidPlan(error) => error.fmt(formatter),
            Self::RevisionConflict => formatter.write_str("Workflow plan revision changed"),
            Self::BudgetExceeded => formatter.write_str("Workflow adjustment budget is exhausted"),
            Self::ReasonRequired => formatter.write_str("A workflow adjustment requires a reason"),
            Self::InvalidChoice(step) => write!(
                formatter,
                "Adjustment does not change a valid parameter of {step:?}"
            ),
            Self::StepLocked(step) => write!(
                formatter,
                "Step {step:?} has already been dispatched or finalized"
            ),
            Self::FallbackNotAllowed(step) => write!(
                formatter,
                "Plan has no approval for this fallback on {step:?}"
            ),
        }
    }
}

impl Error for AdjustmentError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedPlan {
    plan: TemplatePlan,
    plan_revision: u64,
    history: Vec<AdjustmentRecord>,
}

impl AcceptedPlan {
    pub fn accept(
        input: &WorkflowInput,
        policy: &PlanPolicy,
        plan: TemplatePlan,
    ) -> Result<Self, PlanError> {
        validate_plan(input, policy, &plan)?;
        Ok(Self {
            plan,
            plan_revision: 1,
            history: Vec::new(),
        })
    }

    pub fn plan(&self) -> &TemplatePlan {
        &self.plan
    }

    pub fn plan_revision(&self) -> u64 {
        self.plan_revision
    }

    pub fn adjustment_count(&self) -> u8 {
        self.history.len() as u8
    }

    pub fn history(&self) -> &[AdjustmentRecord] {
        &self.history
    }

    /// Dispatch and human approval remain separate Core decisions. Here a
    /// retry or a request for review only changes the adjustment history.
    pub fn adjust(
        &mut self,
        input: &WorkflowInput,
        policy: &PlanPolicy,
        locked_steps: &BTreeSet<Step>,
        proposal: PlanAdjustment,
    ) -> Result<AdjustmentEffect, AdjustmentError> {
        if proposal.expected_revision != self.plan_revision {
            return Err(AdjustmentError::RevisionConflict);
        }
        let reason = proposal.reason.trim();
        if reason.is_empty() {
            return Err(AdjustmentError::ReasonRequired);
        }
        validate_plan(input, policy, &self.plan).map_err(AdjustmentError::InvalidPlan)?;
        if self.adjustment_count() >= self.plan.max_adjustments {
            return Err(AdjustmentError::BudgetExceeded);
        }
        let step = proposal.action.step();
        if locked_steps.contains(&step)
            && !matches!(proposal.action, AdjustmentAction::RequestHumanReview { .. })
        {
            return Err(AdjustmentError::StepLocked(step));
        }

        let mut candidate = self.plan.clone();
        let effect = apply(&mut candidate, proposal.action)?;
        if let AdjustmentEffect::Retry(step) = effect {
            let limit = match enabled_step(&mut candidate, step)? {
                EnabledStep::VisualReview { model_calls, .. } => usize::from(*model_calls - 1),
                _ => 2,
            };
            let retries = self
                .history
                .iter()
                .filter(|record| record.action == AdjustmentAction::RetrySafeStep { step })
                .count();
            if retries >= limit {
                return Err(AdjustmentError::BudgetExceeded);
            }
        }
        validate_plan(input, policy, &candidate).map_err(AdjustmentError::InvalidPlan)?;
        if effect == AdjustmentEffect::PlanChanged {
            self.plan = candidate;
            self.plan_revision += 1;
        }
        self.history.push(AdjustmentRecord {
            resulting_plan_revision: self.plan_revision,
            reason: reason.to_owned(),
            action: proposal.action,
        });
        Ok(effect)
    }
}

impl AdjustmentAction {
    fn step(self) -> Step {
        match self {
            Self::EnableCheck(item) => item.step(),
            Self::ChangeAllowedModel { step, .. }
            | Self::ReduceBatch { step, .. }
            | Self::RetrySafeStep { step }
            | Self::RequestHumanReview { step }
            | Self::SkipWithReason { step } => step,
        }
    }
}

fn apply(
    plan: &mut TemplatePlan,
    action: AdjustmentAction,
) -> Result<AdjustmentEffect, AdjustmentError> {
    match action {
        AdjustmentAction::EnableCheck(item) => plan.enabled_steps.push(item),
        AdjustmentAction::ChangeAllowedModel {
            step,
            model_profile_id,
        } => {
            let item = enabled_step(plan, step)?;
            let EnabledStep::VisualReview {
                model_profile_id: current,
                ..
            } = item
            else {
                return Err(AdjustmentError::InvalidChoice(step));
            };
            if *current == model_profile_id {
                return Err(AdjustmentError::InvalidChoice(step));
            }
            *current = model_profile_id;
        }
        AdjustmentAction::ReduceBatch { step, sample_limit } => {
            let item = enabled_step(plan, step)?;
            let current = match item {
                EnabledStep::DatasetProfile { sample_limit }
                | EnabledStep::AnnotationGeometry { sample_limit }
                | EnabledStep::ImageQuality { sample_limit }
                | EnabledStep::DetectorPredictions { sample_limit }
                | EnabledStep::VisualReview { sample_limit, .. }
                | EnabledStep::StagingQuality { sample_limit } => sample_limit,
                _ => return Err(AdjustmentError::InvalidChoice(step)),
            };
            if sample_limit == 0 || sample_limit >= *current {
                return Err(AdjustmentError::InvalidChoice(step));
            }
            *current = sample_limit;
        }
        AdjustmentAction::RetrySafeStep { step } => {
            require_fallback(plan, step, Fallback::RetrySafeStep)?;
            return Ok(AdjustmentEffect::Retry(step));
        }
        AdjustmentAction::RequestHumanReview { step } => {
            require_fallback(plan, step, Fallback::RequestHumanReview)?;
            return Ok(AdjustmentEffect::WaitForHuman(step));
        }
        AdjustmentAction::SkipWithReason { step } => {
            require_fallback(plan, step, Fallback::SkipWithReason)?;
            plan.enabled_steps.retain(|item| item.step() != step);
            plan.allowed_fallbacks.remove(&step);
        }
    }
    Ok(AdjustmentEffect::PlanChanged)
}

fn enabled_step(plan: &mut TemplatePlan, step: Step) -> Result<&mut EnabledStep, AdjustmentError> {
    plan.enabled_steps
        .iter_mut()
        .find(|item| item.step() == step)
        .ok_or(AdjustmentError::InvalidChoice(step))
}

fn require_fallback(
    plan: &TemplatePlan,
    step: Step,
    fallback: Fallback,
) -> Result<(), AdjustmentError> {
    if plan
        .allowed_fallbacks
        .get(&step)
        .is_some_and(|values| values.contains(&fallback))
    {
        Ok(())
    } else {
        Err(AdjustmentError::FallbackNotAllowed(step))
    }
}
