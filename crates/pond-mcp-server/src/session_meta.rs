//! The calling session from the `_meta` Goose stamps on each call: the only trustworthy
//! per-call channel (a `session_id` param is model-filled; `current_session_id()` races).

use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::user_data::domain::profile::ProfileScope;
use rmcp::model::Meta;
use std::sync::Arc;

/// Goose's `SESSION_ID_HEADER`, duplicated: this crate must not depend on the goose submodule.
pub const SESSION_ID_META_KEY: &str = "agent-session-id";

/// The engine session id behind this call. Key matched case-insensitively, as Goose does;
/// blank is `None`, or every unbound caller would share one session.
pub fn session_from_meta(meta: &Meta) -> Option<String> {
    meta.0
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(SESSION_ID_META_KEY))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The household member speaking in this call, or why there is none. An unidentified
/// speaker in a one-member household can only be that member; a guest is nobody's.
pub async fn speaking_member(
    authority: Option<Arc<dyn DraftAuthority>>,
    meta: &Meta,
) -> Result<String, &'static str> {
    const NOT_ONE_MEMBER: &str = "the speaker is not identified as one household member";
    let authority = authority.ok_or("this pond cannot tell who is speaking")?;
    let session = session_from_meta(meta).ok_or("this call carries no session")?;
    match authority.actor_for_engine_session(&session).await {
        Some((ProfileScope::Owner(id), _)) => Ok(id),
        Some((ProfileScope::Household, _)) => authority.sole_member().await.ok_or(NOT_ONE_MEMBER),
        _ => Err(NOT_ONE_MEMBER),
    }
}

/// A speaker resolver and a call's `_meta`, for the tools that act for the person speaking.
#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use async_trait::async_trait;
    use pond_core::security::ports::policy::{PolicyDecision, PolicyMode};
    use pond_core::user_data::domain::session::IdentificationSource;

    /// Resolves every session to `scope`; `sole` is the household's only member, if it has one.
    pub(crate) struct FixedSpeaker {
        pub(crate) scope: Option<ProfileScope>,
        pub(crate) sole: Option<&'static str>,
    }

    #[async_trait]
    impl DraftAuthority for FixedSpeaker {
        async fn policy_mode(&self) -> PolicyMode {
            PolicyMode::Audit
        }
        async fn actor_for_engine_session(
            &self,
            _engine_session_id: &str,
        ) -> Option<(ProfileScope, IdentificationSource)> {
            self.scope
                .clone()
                .map(|scope| (scope, IdentificationSource::Explicit))
        }
        async fn sole_member(&self) -> Option<String> {
            self.sole.map(str::to_string)
        }
        async fn audit(&self, _s: &str, _a: &str, _d: &PolicyDecision) {}
    }

    pub(crate) fn meta() -> Meta {
        let mut m = Meta::new();
        m.0.insert(
            crate::session_meta::SESSION_ID_META_KEY.to_string(),
            serde_json::json!("engine-1"),
        );
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn meta_with(key: &str, value: &str) -> Meta {
        let mut m = Meta::new();
        m.0.insert(key.to_string(), Value::String(value.to_string()));
        m
    }

    #[test]
    fn reads_the_engine_key() {
        assert_eq!(
            session_from_meta(&meta_with(SESSION_ID_META_KEY, "20260805_7")),
            Some("20260805_7".to_string())
        );
    }

    #[test]
    fn matches_the_key_case_insensitively() {
        assert_eq!(
            session_from_meta(&meta_with("Agent-Session-Id", "20260805_7")),
            Some("20260805_7".to_string())
        );
    }

    #[test]
    fn blank_and_absent_are_both_none() {
        assert_eq!(
            session_from_meta(&meta_with(SESSION_ID_META_KEY, "   ")),
            None
        );
        assert_eq!(session_from_meta(&Meta::new()), None);
        assert_eq!(session_from_meta(&meta_with("progressToken", "x")), None);
    }
}
