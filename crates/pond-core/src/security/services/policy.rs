//! The default allow-all [`SecurityPolicy`].

use crate::security::ports::policy::{PolicyDecision, Principal, SecurityPolicy};
use anyhow::Result;
use async_trait::async_trait;

/// Allows every access; audit only emits a `tracing::debug!` line.
#[derive(Debug, Default, Clone)]
pub struct AllowAllPolicy;

impl AllowAllPolicy {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecurityPolicy for AllowAllPolicy {
    async fn allow(&self, _principal: &Principal, _scope: &str) -> Result<bool> {
        Ok(true)
    }

    /// Logs the verdict, not just the effect: `ok` alone reads "permitted" for would-denies.
    async fn audit(
        &self,
        principal: &Principal,
        action: &str,
        scope: &str,
        decision: &PolicyDecision,
    ) {
        tracing::debug!(
            ?principal,
            action,
            scope,
            ok = decision.allowed,
            verdict = decision.verdict(),
            mode = decision.mode.as_str(),
            reason = decision.denied_reason.unwrap_or(""),
            "security audit (allow-all policy)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ports::policy::{
        scopes, PolicyMode, PrincipalKind, REASON_UNPROVEN_IDENTITY,
    };

    #[tokio::test]
    async fn allow_returns_true_for_every_principal_kind() {
        let policy = AllowAllPolicy::new();
        let principals = [
            Principal::loopback(),
            Principal::internal(),
            Principal::token("client-abc"),
        ];
        for principal in principals {
            assert!(policy.allow(&principal, scopes::MEMORY).await.unwrap());
            assert!(policy.allow(&principal, scopes::SECRETS).await.unwrap());
        }
    }

    #[tokio::test]
    async fn allow_returns_true_for_unknown_scope() {
        let policy = AllowAllPolicy::default();
        assert!(policy
            .allow(&Principal::loopback(), "some-future-scope")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn audit_does_not_panic() {
        let policy = AllowAllPolicy::new();
        let principal = Principal::token("client-xyz").with_remote_addr("10.0.0.5:55123");
        policy
            .audit(
                &principal,
                "read",
                scopes::PROFILE,
                &PolicyDecision::permit(PolicyMode::Audit),
            )
            .await;
        policy
            .audit(
                &Principal::internal(),
                "write",
                scopes::SCHEDULE,
                &PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY),
            )
            .await;
        policy
            .audit(
                &Principal::internal(),
                "write",
                scopes::SCHEDULE,
                &PolicyDecision::refuse(PolicyMode::Enforce, REASON_UNPROVEN_IDENTITY),
            )
            .await;
    }

    #[test]
    fn principal_constructors_set_expected_kind() {
        assert_eq!(Principal::loopback().kind, PrincipalKind::Loopback);
        assert_eq!(Principal::internal().kind, PrincipalKind::Internal);
        assert_eq!(
            Principal::token("c1").kind,
            PrincipalKind::Token("c1".to_string())
        );
    }
}
