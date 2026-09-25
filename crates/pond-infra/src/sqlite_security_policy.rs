//! SQLite-backed [`SecurityPolicy`] adapter.
//!
//! Only identity assertion and draft approve/reject audit through it; the rest is unaudited.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::policy::{
    audit_attrs, PolicyDecision, Principal, PrincipalKind, SecurityPolicy, AUDIT_ACTION,
};

/// Default-allow [`SecurityPolicy`]; audits go to the unified event log as `Auth` events.
pub struct SqliteSecurityPolicy {
    event_log: Arc<dyn EventLog>,
}

impl SqliteSecurityPolicy {
    pub fn new(event_log: Arc<dyn EventLog>) -> Self {
        Self { event_log }
    }
}

/// Render a [`Principal`] into a short, stable token for audit messages.
fn principal_label(principal: &Principal) -> String {
    match &principal.kind {
        PrincipalKind::Loopback => "loopback".to_string(),
        PrincipalKind::Internal => "internal".to_string(),
        PrincipalKind::Token(client_id) => format!("token:{client_id}"),
    }
}

#[async_trait]
impl SecurityPolicy for SqliteSecurityPolicy {
    async fn allow(&self, _principal: &Principal, _scope: &str) -> Result<bool> {
        // Hook, not a gate: no authorization rules exist yet.
        Ok(true)
    }

    async fn audit(
        &self,
        principal: &Principal,
        action: &str,
        scope: &str,
        decision: &PolicyDecision,
    ) {
        // Sensitive: `remote_addr` and `token:<client_id>` identify a device.
        // `ok` stays beside `verdict`: in `audit` mode `ok` is true even for a would-deny.
        let mut event = Event::new(EventCategory::Auth, AUDIT_ACTION)
            .attr(audit_attrs::PRINCIPAL, principal_label(principal))
            .attr(audit_attrs::ACTION, action)
            .attr(audit_attrs::SCOPE, scope)
            .attr(audit_attrs::OK, decision.allowed)
            .attr(audit_attrs::VERDICT, decision.verdict())
            .attr(audit_attrs::MODE, decision.mode.as_str())
            .sensitivity(PrivacySensitivity::Sensitive);
        if let Some(reason) = decision.denied_reason {
            // Absent, not blank: an empty reason reads as one we failed to record.
            event = event.attr(audit_attrs::REASON, reason);
        }
        if let Some(addr) = &principal.remote_addr {
            event = event.attr(audit_attrs::REMOTE_ADDR, addr.as_str());
        }

        // Auditing must never fail the caller; log and swallow any error.
        if let Err(e) = self.event_log.append(event).await {
            tracing::warn!(error = %e, "failed to write security audit entry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_event_log::SqliteEventLog;
    use pond_core::security::domain::event::EventQuery;
    use pond_core::security::ports::policy::{scopes, PolicyMode, REASON_UNPROVEN_IDENTITY};
    use tempfile::tempdir;

    fn permit() -> PolicyDecision {
        PolicyDecision::permit(PolicyMode::Audit)
    }

    async fn make_policy() -> (SqliteSecurityPolicy, Arc<dyn EventLog>, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let event_log: Arc<dyn EventLog> = Arc::new(SqliteEventLog::new(db.logs));
        let policy = SqliteSecurityPolicy::new(event_log.clone());
        (policy, event_log, tmp)
    }

    #[tokio::test]
    async fn allow_returns_true() {
        let (policy, _log, _tmp) = make_policy().await;
        assert!(policy
            .allow(&Principal::loopback(), scopes::MEMORY)
            .await
            .unwrap());
        assert!(policy
            .allow(&Principal::token("c1"), scopes::SECRETS)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn audit_appends_a_typed_auth_event() {
        let (policy, event_log, _tmp) = make_policy().await;
        let principal = Principal::token("abc").with_remote_addr("10.0.0.2:5000");

        policy
            .audit(&principal, "read", scopes::MEMORY, &permit())
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.category, EventCategory::Auth);
        assert_eq!(ev.action, AUDIT_ACTION);
        assert_eq!(ev.attributes.get("principal"), Some(&"token:abc".into()));
        assert_eq!(ev.attributes.get("action"), Some(&"read".into()));
        assert_eq!(ev.attributes.get("scope"), Some(&scopes::MEMORY.into()));
        assert_eq!(ev.attributes.get("ok"), Some(&true.into()));
        assert_eq!(ev.attributes.get("verdict"), Some(&"allow".into()));
        assert_eq!(ev.attributes.get("mode"), Some(&"audit".into()));
        assert!(
            !ev.attributes.contains_key("reason"),
            "a permit has no reason; a blank one reads as a lost field"
        );
        assert_eq!(
            ev.attributes.get("remote_addr"),
            Some(&"10.0.0.2:5000".into())
        );
    }

    #[tokio::test]
    async fn a_would_deny_is_distinguishable_from_an_allow_in_the_stored_event() {
        let (policy, event_log, _tmp) = make_policy().await;
        let principal = Principal::token("phone");

        policy
            .audit(&principal, "identify_session", scopes::SESSION, &permit())
            .await;
        policy
            .audit(
                &principal,
                "identify_session",
                scopes::SESSION,
                &PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY),
            )
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(events.len(), 2);
        let mut verdicts: Vec<_> = events
            .iter()
            .map(|e| e.attributes.get("verdict").cloned().unwrap())
            .collect();
        verdicts.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(verdicts, vec!["allow".into(), "would_deny".into()]);
        assert!(
            events
                .iter()
                .all(|e| e.attributes.get("ok") == Some(&true.into())),
            "audit mode blocks nothing, so `ok` cannot be the discriminator"
        );
        let refused = events
            .iter()
            .find(|e| e.attributes.get("verdict") == Some(&"would_deny".into()))
            .unwrap();
        assert_eq!(
            refused.attributes.get("reason"),
            Some(&REASON_UNPROVEN_IDENTITY.into())
        );
    }

    #[tokio::test]
    async fn the_action_string_does_not_carry_the_verdict() {
        let (policy, event_log, _tmp) = make_policy().await;
        policy
            .audit(
                &Principal::token("phone"),
                "identify_session",
                scopes::SESSION,
                &PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY),
            )
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].attributes.get("action"),
            Some(&"identify_session".into())
        );
    }

    #[tokio::test]
    async fn audit_events_are_classified_sensitive() {
        let (policy, event_log, _tmp) = make_policy().await;

        policy
            .audit(
                &Principal::token("abc").with_remote_addr("10.0.0.2:5000"),
                "read",
                scopes::MEMORY,
                &permit(),
            )
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].privacy_sensitivity,
            PrivacySensitivity::Sensitive,
            "audit events identify a device and must not be downgraded"
        );
    }

    /// Both logs share `pond_logs.db`, so only the code keeps them apart.
    #[tokio::test]
    async fn audit_does_not_write_to_the_operational_log() {
        use crate::sqlite_event_log::SqliteOperationalLog;
        use pond_core::security::ports::event_log::OperationalLogRepository;

        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let policy = SqliteSecurityPolicy::new(Arc::new(SqliteEventLog::new(db.logs.clone())));
        let operational = SqliteOperationalLog::new(db.logs.clone());

        policy
            .audit(&Principal::token("abc"), "read", scopes::MEMORY, &permit())
            .await;

        let rows = operational.list(50, None).await.unwrap();
        assert!(
            rows.is_empty(),
            "audit events must not reach the operational log: {rows:?}"
        );
    }

    #[tokio::test]
    async fn audit_omits_remote_addr_when_there_is_none() {
        let (policy, event_log, _tmp) = make_policy().await;

        policy
            .audit(&Principal::loopback(), "read", scopes::MEMORY, &permit())
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].attributes.get("principal"),
            Some(&"loopback".into())
        );
        assert!(!events[0].attributes.contains_key("remote_addr"));
    }
}
