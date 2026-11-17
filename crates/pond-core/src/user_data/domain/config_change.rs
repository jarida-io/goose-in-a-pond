//! A change to household configuration (a setting, an extension, a schedule) and how much of it
//! may be written to the logs. Pure: recording lives in
//! [`crate::user_data::services::audited_settings_repository`].

use std::collections::HashSet;

use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};

/// Longest value, in characters, a log line or activity event carries before it is cut.
pub const MAX_LOGGED_VALUE_CHARS: usize = 120;

/// Settings that describe the household itself. A change is recorded, its values are not.
const PERSONAL_KEYS: &[&str] = &[
    "user_name",
    "home_name",
    "weather_latitude",
    "weather_longitude",
    "weather_location_name",
    "assistant_personality",
    "primary_profile_id",
    "voice_wake_word_transcriptions",
    "custom_system_prompt",
    "prompt_addendum",
];

/// Settings that are credentials however their name reads.
const SECRET_KEYS: &[&str] = &["mesh_identity_secret", "lightning_wallet_mnemonic"];

/// Underscore-separated name segments that mark a credential. Matched per segment, not as a
/// substring, so `llm_max_tokens` keeps its value while `hf_token` loses it.
const SECRET_SEGMENTS: &[&str] = &[
    "token",
    "authtoken",
    "secret",
    "password",
    "passphrase",
    "mnemonic",
    "credential",
    "credentials",
];

/// What sort of configuration changed; picks the trace `kind` and the activity action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigChangeKind {
    /// A row in the settings table.
    Setting,
    /// An MCP extension added, removed, enabled or disabled.
    Extension,
    /// A schedule or rule paused or resumed.
    Schedule,
}

impl ConfigChangeKind {
    /// The `kind` field of the `giap::trace` line.
    pub fn trace_kind(self) -> &'static str {
        match self {
            Self::Setting => "setting_changed",
            Self::Extension => "extension_changed",
            Self::Schedule => "schedule_changed",
        }
    }

    /// The activity-log action name.
    pub fn action(self) -> &'static str {
        match self {
            Self::Setting => "settings.changed",
            Self::Extension => "extension.changed",
            Self::Schedule => "schedule.changed",
        }
    }
}

/// How much of a changed value may be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disclosure {
    /// Old and new values are logged, cut to [`MAX_LOGGED_VALUE_CHARS`].
    Plain,
    /// Personal to the household: the change is logged, the values are not.
    Personal,
    /// A credential: the change is logged, the values are not.
    Secret,
}

impl Disclosure {
    /// Classify a key by name. Secrets are checked first, so a personal-looking key that also
    /// names a credential stays withheld either way.
    pub fn for_key(key: &str) -> Self {
        let lower = key.to_ascii_lowercase();
        let secret = SECRET_KEYS.contains(&lower.as_str())
            || lower.starts_with("api_key")
            || lower.split('_').any(|seg| SECRET_SEGMENTS.contains(&seg));
        if secret {
            Self::Secret
        } else if PERSONAL_KEYS.contains(&lower.as_str()) {
            Self::Personal
        } else {
            Self::Plain
        }
    }

    /// Whether values are withheld from logs and events.
    pub fn withholds_values(self) -> bool {
        !matches!(self, Self::Plain)
    }
}

/// One configuration value going from `from` to `to`; `None` means unset or absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChange {
    pub kind: ConfigChangeKind,
    pub key: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

impl ConfigChange {
    /// A change of `key`, in the raw form it is stored in.
    pub fn new(
        kind: ConfigChangeKind,
        key: impl Into<String>,
        from: Option<String>,
        to: Option<String>,
    ) -> Self {
        Self {
            kind,
            key: key.into(),
            from,
            to,
        }
    }

    /// How much of this change may be logged.
    pub fn disclosure(&self) -> Disclosure {
        Disclosure::for_key(&self.key)
    }

    /// The old and new values safe to log, or `None` when they are withheld.
    pub fn loggable_values(&self) -> Option<(Option<String>, Option<String>)> {
        if self.disclosure().withholds_values() {
            return None;
        }
        Some((
            self.from.as_deref().map(loggable_value),
            self.to.as_deref().map(loggable_value),
        ))
    }

    /// The activity event, with attributes `key`, `from`, `to` and `redacted` (the names the
    /// desktop's activity view reads). A personal change is raised to `Sensitive`.
    pub fn to_event(&self) -> Event {
        let mut event =
            Event::new(EventCategory::System, self.kind.action()).attr("key", &*self.key);
        match self.loggable_values() {
            Some((from, to)) => {
                if let Some(from) = from {
                    event = event.attr("from", from);
                }
                if let Some(to) = to {
                    event = event.attr("to", to);
                }
            }
            None => event = event.attr("redacted", true),
        }
        if self.disclosure() == Disclosure::Personal {
            event = event.sensitivity(PrivacySensitivity::Sensitive);
        }
        event
    }
}

/// A value as it may appear in a log: URL credentials removed, then cut to
/// [`MAX_LOGGED_VALUE_CHARS`] with a trailing `...`.
pub fn loggable_value(value: &str) -> String {
    let value = strip_url_userinfo(value);
    if value.chars().count() <= MAX_LOGGED_VALUE_CHARS {
        return value;
    }
    let cut: String = value.chars().take(MAX_LOGGED_VALUE_CHARS).collect();
    format!("{cut}...")
}

/// `rtsp://user:pass@cam/stream` becomes `rtsp://***@cam/stream`; anything else is unchanged.
fn strip_url_userinfo(value: &str) -> String {
    let Some(scheme_end) = value.find("://") else {
        return value.to_string();
    };
    let rest = &value[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{}://***@{}", &value[..scheme_end], &rest[at + 1..]),
        None => value.to_string(),
    }
}

/// The settings that differ between two serialized `Settings` snapshots, limited to `only` when
/// given. Keys are the serde field names, which are the settings-table keys.
pub fn diff_settings(
    before: &serde_json::Value,
    after: &serde_json::Value,
    only: Option<&HashSet<String>>,
) -> Vec<ConfigChange> {
    let Some(after) = after.as_object() else {
        return Vec::new();
    };
    let mut changes: Vec<ConfigChange> = after
        .iter()
        .filter(|(key, _)| only.is_none_or(|keys| keys.contains(*key)))
        .filter_map(|(key, new)| {
            let old = before.get(key).unwrap_or(&serde_json::Value::Null);
            (old != new).then(|| {
                ConfigChange::new(
                    ConfigChangeKind::Setting,
                    key.clone(),
                    render(old),
                    render(new),
                )
            })
        })
        .collect();
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes
}

/// A serialized setting as text; `null` (an unset optional) is `None`.
fn render(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::domain::event::AttributeValue;
    use serde_json::json;

    #[test]
    fn credentials_are_secret_by_name() {
        for key in [
            "mesh_identity_secret",
            "lightning_wallet_mnemonic",
            "api_key_gnews",
            "hf_token",
            "db_password",
            "client_secret",
        ] {
            assert_eq!(Disclosure::for_key(key), Disclosure::Secret, "{key}");
        }
    }

    #[test]
    fn a_token_count_is_not_a_credential() {
        assert_eq!(Disclosure::for_key("llm_max_tokens"), Disclosure::Plain);
    }

    #[test]
    fn household_values_are_personal() {
        for key in PERSONAL_KEYS {
            assert_eq!(Disclosure::for_key(key), Disclosure::Personal, "{key}");
        }
        assert_eq!(Disclosure::for_key("assistant_name"), Disclosure::Plain);
    }

    #[test]
    fn long_values_are_cut() {
        let long = "x".repeat(MAX_LOGGED_VALUE_CHARS + 50);
        let logged = loggable_value(&long);
        assert_eq!(logged.chars().count(), MAX_LOGGED_VALUE_CHARS + 3);
        assert!(logged.ends_with("..."));
    }

    #[test]
    fn url_credentials_never_reach_a_log() {
        assert_eq!(
            loggable_value("rtsp://viewer:pw@10.0.0.5:554/stream"),
            "rtsp://***@10.0.0.5:554/stream"
        );
        assert_eq!(
            loggable_value("http://localhost:5580/ws"),
            "http://localhost:5580/ws"
        );
        assert_eq!(loggable_value("someone@example"), "someone@example");
    }

    #[test]
    fn a_secret_event_carries_no_values() {
        let change = ConfigChange::new(
            ConfigChangeKind::Setting,
            "api_key_gnews",
            None,
            Some("sk-live".into()),
        );
        let event = change.to_event();
        assert_eq!(event.action, "settings.changed");
        assert_eq!(
            event.attributes.get("redacted"),
            Some(&AttributeValue::Bool(true))
        );
        assert!(!event.attributes.contains_key("from"));
        assert!(!event.attributes.contains_key("to"));
    }

    #[test]
    fn a_personal_event_is_sensitive_and_carries_no_values() {
        let change = ConfigChange::new(
            ConfigChangeKind::Setting,
            "user_name",
            Some("A".into()),
            Some("B".into()),
        );
        let event = change.to_event();
        assert_eq!(event.privacy_sensitivity, PrivacySensitivity::Sensitive);
        assert!(!event.attributes.contains_key("to"));
    }

    #[test]
    fn diff_reports_only_changed_keys_within_the_patch() {
        let before = json!({"a": 1, "b": "x", "c": null});
        let after = json!({"a": 2, "b": "x", "c": "set"});
        let all = diff_settings(&before, &after, None);
        let keys: Vec<&str> = all.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["a", "c"]);
        assert_eq!(all[1].from, None);
        assert_eq!(all[1].to.as_deref(), Some("set"));

        let only: HashSet<String> = ["c".to_string()].into_iter().collect();
        let patch = diff_settings(&before, &after, Some(&only));
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0].key, "c");
    }
}
