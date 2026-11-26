//! Workspace-scoped approvals for a previewed import plan.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display};

use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportApprovalError {
    UnknownPlan,
    FingerprintMismatch,
    Busy,
}

impl Display for ImportApprovalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownPlan => "Import plan is no longer available in this Workspace session",
            Self::FingerprintMismatch => "Import confirmation does not match the preview",
            Self::Busy => "This import plan is already being applied",
        })
    }
}

impl Error for ImportApprovalError {}

pub enum Approval<P, R> {
    Apply(P),
    Replay(R),
}

enum State<P, R> {
    Previewed(P),
    Applying,
    Completed(R),
}

struct Plan<P, R> {
    session: Uuid,
    fingerprint: [u8; 32],
    state: State<P, R>,
}

pub struct ImportApprovalBook<P, R> {
    plans: HashMap<Uuid, Plan<P, R>>,
}

impl<P, R> Default for ImportApprovalBook<P, R> {
    fn default() -> Self {
        Self {
            plans: HashMap::new(),
        }
    }
}

impl<P: Clone, R: Clone> ImportApprovalBook<P, R> {
    pub fn remember(&mut self, session: Uuid, fingerprint: [u8; 32], preview: P) -> Uuid {
        self.plans.retain(|_, plan| plan.session == session);
        let plan_id = Uuid::new_v4();
        self.plans.insert(
            plan_id,
            Plan {
                session,
                fingerprint,
                state: State::Previewed(preview),
            },
        );
        plan_id
    }

    pub fn confirm(
        &mut self,
        session: Uuid,
        plan_id: Uuid,
        fingerprint: [u8; 32],
    ) -> Result<Approval<P, R>, ImportApprovalError> {
        let plan = self
            .plans
            .get_mut(&plan_id)
            .filter(|plan| plan.session == session)
            .ok_or(ImportApprovalError::UnknownPlan)?;
        if plan.fingerprint != fingerprint {
            return Err(ImportApprovalError::FingerprintMismatch);
        }
        match &plan.state {
            State::Previewed(preview) => {
                let preview = preview.clone();
                plan.state = State::Applying;
                Ok(Approval::Apply(preview))
            }
            State::Applying => Err(ImportApprovalError::Busy),
            State::Completed(receipt) => Ok(Approval::Replay(receipt.clone())),
        }
    }

    pub fn complete(&mut self, id: Uuid, receipt: R) {
        if let Some(plan) = self.plans.get_mut(&id) {
            plan.state = State::Completed(receipt);
        }
    }

    pub fn retry(&mut self, id: Uuid, preview: P) {
        if let Some(plan) = self.plans.get_mut(&id) {
            plan.state = State::Previewed(preview);
        }
    }

    pub fn discard(&mut self, id: Uuid) {
        self.plans.remove(&id);
    }
}
