//! In-memory authorization for dispatch attempts in one active Workspace session.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{self, Display};
use std::time::Instant;

use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantScope {
    pub workspace_session: Uuid,
    pub task: Uuid,
    pub attempt: Uuid,
    pub executor_session: Uuid,
    pub input_fingerprint: [u8; 32],
    pub operation_digest: [u8; 32],
    pub reservation: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimContext {
    pub workspace_session: Uuid,
    pub task: Uuid,
    pub attempt: Uuid,
    pub execution: Uuid,
    pub input_fingerprint: [u8; 32],
    pub operation_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimReceipt {
    pub task: Uuid,
    pub attempt: Uuid,
    pub execution: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrantError {
    UnknownGrant,
    ScopeMismatch,
    TaskActive,
    RetiredAttempt,
    Expired,
    AlreadyClaimed,
    Unclaimed,
}

impl Display for GrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownGrant => "Execution grant is no longer current",
            Self::ScopeMismatch => "Execution grant scope does not match the request",
            Self::TaskActive => "Task already has a dispatch grant",
            Self::RetiredAttempt => "Attempt may only be authorized once",
            Self::Expired => "Execution grant expired before it was claimed",
            Self::AlreadyClaimed => "Another execution has already claimed this grant",
            Self::Unclaimed => "No execution has claimed this grant",
        })
    }
}

impl Error for GrantError {}

struct Grant {
    scope: GrantScope,
    expires_at: Instant,
    receipt: Option<ClaimReceipt>,
}

impl Grant {
    fn verify(&self, authenticated_executor: Uuid, claim: &ClaimContext) -> Result<(), GrantError> {
        if self.scope.executor_session != authenticated_executor
            || self.scope.workspace_session != claim.workspace_session
            || self.scope.task != claim.task
            || self.scope.attempt != claim.attempt
            || self.scope.input_fingerprint != claim.input_fingerprint
            || self.scope.operation_digest != claim.operation_digest
        {
            return Err(GrantError::ScopeMismatch);
        }
        Ok(())
    }
}

pub struct GrantBook {
    workspace_session: Uuid,
    grants: HashMap<Uuid, Grant>,
    active_tasks: HashSet<Uuid>,
    issued_attempts: HashSet<Uuid>,
}

impl GrantBook {
    pub fn new(workspace_session: Uuid) -> Self {
        Self {
            workspace_session,
            grants: HashMap::new(),
            active_tasks: HashSet::new(),
            issued_attempts: HashSet::new(),
        }
    }

    /// Stage a grant locally. The scheduler can send its identifier only after
    /// persisting the current DISPATCHING attempt with its reservation.
    pub fn install(&mut self, scope: GrantScope, expires_at: Instant) -> Result<Uuid, GrantError> {
        if scope.workspace_session != self.workspace_session {
            return Err(GrantError::ScopeMismatch);
        }
        if self.active_tasks.contains(&scope.task) {
            return Err(GrantError::TaskActive);
        }
        if !self.issued_attempts.insert(scope.attempt) {
            return Err(GrantError::RetiredAttempt);
        }
        let id = Uuid::new_v4();
        self.active_tasks.insert(scope.task);
        self.grants.insert(
            id,
            Grant {
                scope,
                expires_at,
                receipt: None,
            },
        );
        Ok(id)
    }

    pub fn claim(
        &mut self,
        id: Uuid,
        authenticated_executor: Uuid,
        claim: &ClaimContext,
        now: Instant,
    ) -> Result<ClaimReceipt, GrantError> {
        let receipt = self.validate_claim(id, authenticated_executor, claim, now)?;
        self.grants
            .get_mut(&id)
            .ok_or(GrantError::UnknownGrant)?
            .receipt = Some(receipt);
        Ok(receipt)
    }

    /// Validate an execution receipt before writing it to the authoritative Task row.
    pub fn validate_claim(
        &self,
        id: Uuid,
        authenticated_executor: Uuid,
        claim: &ClaimContext,
        now: Instant,
    ) -> Result<ClaimReceipt, GrantError> {
        let grant = self.grants.get(&id).ok_or(GrantError::UnknownGrant)?;
        grant.verify(authenticated_executor, claim)?;
        if let Some(receipt) = grant.receipt {
            return if receipt.execution == claim.execution {
                Ok(receipt)
            } else {
                Err(GrantError::AlreadyClaimed)
            };
        }
        if now >= grant.expires_at {
            return Err(GrantError::Expired);
        }
        Ok(ClaimReceipt {
            task: claim.task,
            attempt: claim.attempt,
            execution: claim.execution,
        })
    }

    pub fn expire_unclaimed(&mut self, id: Uuid, now: Instant) -> Option<Uuid> {
        let grant = self.grants.get(&id)?;
        if grant.receipt.is_some() || now < grant.expires_at {
            return None;
        }
        self.remove(id)
    }

    /// Call only after the executor has stopped and Core has accepted its end.
    pub fn release_finished(
        &mut self,
        id: Uuid,
        authenticated_executor: Uuid,
        claim: &ClaimContext,
    ) -> Result<Uuid, GrantError> {
        let grant = self.grants.get(&id).ok_or(GrantError::UnknownGrant)?;
        grant.verify(authenticated_executor, claim)?;
        let receipt = grant.receipt.ok_or(GrantError::Unclaimed)?;
        if receipt.execution != claim.execution {
            return Err(GrantError::AlreadyClaimed);
        }
        self.remove(id).ok_or(GrantError::UnknownGrant)
    }

    fn remove(&mut self, id: Uuid) -> Option<Uuid> {
        let grant = self.grants.remove(&id)?;
        self.active_tasks.remove(&grant.scope.task);
        Some(grant.scope.reservation)
    }
}
