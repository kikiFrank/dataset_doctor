use std::time::{Duration, Instant};

use dd_core::execution_grant::{ClaimContext, GrantBook, GrantError, GrantScope};
use uuid::Uuid;

fn scope(workspace_session: Uuid) -> GrantScope {
    GrantScope {
        workspace_session,
        task: Uuid::new_v4(),
        attempt: Uuid::new_v4(),
        executor_session: Uuid::new_v4(),
        input_fingerprint: [1; 32],
        operation_digest: [2; 32],
        reservation: Uuid::new_v4(),
    }
}

fn claimant(scope: &GrantScope) -> ClaimContext {
    ClaimContext {
        workspace_session: scope.workspace_session,
        task: scope.task,
        attempt: scope.attempt,
        execution: Uuid::new_v4(),
        input_fingerprint: scope.input_fingerprint,
        operation_digest: scope.operation_digest,
    }
}

#[test]
fn only_an_authenticated_matching_claim_can_start_an_attempt() {
    let scope = scope(Uuid::new_v4());
    let now = Instant::now();
    let mut grants = GrantBook::new(scope.workspace_session);
    let grant = grants
        .install(scope.clone(), now + Duration::from_secs(2))
        .unwrap();
    let mut claim = claimant(&scope);
    assert_eq!(
        grants.claim(grant, Uuid::new_v4(), &claim, now),
        Err(GrantError::ScopeMismatch)
    );
    claim.input_fingerprint = [0; 32];
    assert_eq!(
        grants.claim(grant, scope.executor_session, &claim, now),
        Err(GrantError::ScopeMismatch)
    );
    claim.input_fingerprint = scope.input_fingerprint;
    claim.operation_digest = [0; 32];
    assert_eq!(
        grants.claim(grant, scope.executor_session, &claim, now),
        Err(GrantError::ScopeMismatch)
    );
    claim.operation_digest = scope.operation_digest;
    let original = grants
        .claim(grant, scope.executor_session, &claim, now)
        .unwrap();
    assert_eq!(original.execution, claim.execution);
    assert_eq!(original.attempt, claim.attempt);
    assert_eq!(
        grants.claim(
            grant,
            scope.executor_session,
            &claim,
            now + Duration::from_secs(3)
        ),
        Ok(original)
    );
    claim.execution = Uuid::new_v4();
    assert_eq!(
        grants.claim(grant, scope.executor_session, &claim, now),
        Err(GrantError::AlreadyClaimed)
    );
}

#[test]
fn expiry_revokes_only_unclaimed_grants_and_never_reuses_old_attempt_admission() {
    let scope = scope(Uuid::new_v4());
    let now = Instant::now();
    let expires = now + Duration::from_secs(1);
    let mut grants = GrantBook::new(scope.workspace_session);
    let grant = grants.install(scope.clone(), expires).unwrap();
    let claim = claimant(&scope);
    assert_eq!(
        grants.claim(grant, scope.executor_session, &claim, expires),
        Err(GrantError::Expired)
    );
    assert_eq!(
        grants.expire_unclaimed(grant, expires),
        Some(scope.reservation)
    );
    assert_eq!(grants.expire_unclaimed(grant, expires), None);
    assert_eq!(
        grants.install(scope.clone(), expires),
        Err(GrantError::RetiredAttempt)
    );

    let mut next = scope.clone();
    next.attempt = Uuid::new_v4();
    next.reservation = Uuid::new_v4();
    let replacement = grants
        .install(next.clone(), expires + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        grants.claim(grant, scope.executor_session, &claim, expires),
        Err(GrantError::UnknownGrant)
    );
    let active = claimant(&next);
    grants
        .claim(replacement, next.executor_session, &active, expires)
        .unwrap();
    assert_eq!(
        grants.expire_unclaimed(replacement, expires + Duration::from_secs(3)),
        None
    );
    assert_eq!(grants.install(scope, expires), Err(GrantError::TaskActive));
}

#[test]
fn another_workspace_or_an_old_claim_cannot_release_current_resources() {
    let scope = scope(Uuid::new_v4());
    let now = Instant::now();
    let expires = now + Duration::from_secs(1);
    let mut grants = GrantBook::new(scope.workspace_session);
    let mut foreign = scope.clone();
    foreign.workspace_session = Uuid::new_v4();
    assert_eq!(
        grants.install(foreign, expires),
        Err(GrantError::ScopeMismatch)
    );
    let grant = grants.install(scope.clone(), expires).unwrap();
    let mut claim = claimant(&scope);
    let receipt = grants
        .claim(grant, scope.executor_session, &claim, now)
        .unwrap();
    claim.execution = Uuid::new_v4();
    assert_eq!(
        grants.release_finished(grant, scope.executor_session, &claim),
        Err(GrantError::AlreadyClaimed)
    );
    claim.execution = receipt.execution;
    assert_eq!(
        grants.release_finished(grant, scope.executor_session, &claim),
        Ok(scope.reservation)
    );
    assert_eq!(
        grants.release_finished(grant, scope.executor_session, &claim),
        Err(GrantError::UnknownGrant)
    );
}
