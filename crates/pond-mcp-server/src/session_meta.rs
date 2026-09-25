//! The calling session from the `_meta` Goose stamps on each call: the only trustworthy
//! per-call channel (a `session_id` param is model-filled; `current_session_id()` races).

use rmcp::model::Meta;

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
