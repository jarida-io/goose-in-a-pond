use serde::{Deserialize, Serialize};

/// Describes a secret that an extension needs to function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequirement {
    /// Environment variable name (e.g. "SPOTIFY_ACCESS_TOKEN")
    pub key: String,
    /// Human-readable label (e.g. "Spotify")
    pub display_name: String,
    /// Help text (e.g. "Sign in to control playback")
    pub description: String,
    /// Whether the extension won't work without this secret
    pub required: bool,
    pub kind: SecretKind,
    /// Used by the host only (e.g. a signing key), so never put in the extension's environment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub host_only: bool,
    /// For someone bringing their own credentials or overriding a default. The UI keeps these
    /// under "Developer settings" so the ordinary path is a sign-in button, not a form.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advanced: bool,
    /// For a `choice`: the answers it takes, the first being what applies until one is saved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SecretOption>,
}

/// One answer a `choice` takes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretOption {
    /// What is stored and put in the extension's environment.
    pub value: String,
    /// What a person reads.
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl SecretRequirement {
    /// Whether `value` is one of this choice's answers (anything goes for another kind).
    pub fn accepts(&self, value: &str) -> bool {
        self.kind != SecretKind::Choice || self.options.iter().any(|o| o.value == value)
    }

    /// Whether an extension may not be installed or started while this secret is missing: a
    /// required one the user pastes. A sign-in flow is completed after install, so it never blocks.
    pub fn blocks_install(&self) -> bool {
        self.required && self.kind != SecretKind::OAuthFlow
    }

    /// The value that applies to a choice until one is saved: its first answer.
    pub fn default_value(&self) -> Option<&str> {
        match self.kind {
            SecretKind::Choice => self.options.first().map(|o| o.value.as_str()),
            _ => None,
        }
    }
}

/// How a secret is obtained by the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// Manual paste (GitHub token, Brave API key)
    ApiKey,
    /// One-click "Sign in with X" — GIAP handles the entire OAuth PKCE flow
    #[serde(rename = "oauth_flow")]
    OAuthFlow,
    /// Free-form text input
    Generic,
    /// One of fixed `options`, such as which service to use. Not a secret: its value is read back
    /// and shown, so a person can see what they chose.
    Choice,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice() -> SecretRequirement {
        serde_json::from_str(
            r#"{"key":"MUSIC_SERVICE","display_name":"Music service","description":"","required":false,
                "kind":"choice","options":[{"value":"apple","label":"Apple Music"},
                {"value":"spotify","label":"Spotify","description":"By hand"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_choice_takes_only_its_answers_and_its_first_is_the_default() {
        let c = choice();
        assert_eq!(c.kind, SecretKind::Choice);
        assert!(c.accepts("apple") && c.accepts("spotify"));
        assert!(!c.accepts("tidal") && !c.accepts(""));
        assert_eq!(c.default_value(), Some("apple"));
        assert_eq!(c.options[1].description, "By hand");
    }

    fn requirement(required: bool, kind: &str) -> SecretRequirement {
        serde_json::from_str(&format!(
            r#"{{"key":"K","display_name":"K","description":"","required":{required},"kind":"{kind}"}}"#
        ))
        .unwrap()
    }

    #[test]
    fn only_a_required_secret_the_user_pastes_blocks_install() {
        assert!(requirement(true, "api_key").blocks_install());
        assert!(requirement(true, "generic").blocks_install());
        assert!(
            !requirement(true, "oauth_flow").blocks_install(),
            "a sign-in happens after install"
        );
        for kind in ["api_key", "generic", "oauth_flow", "choice"] {
            assert!(!requirement(false, kind).blocks_install(), "{kind}");
        }
    }

    #[test]
    fn any_other_kind_takes_anything_and_has_no_default() {
        let plain: SecretRequirement = serde_json::from_str(
            r#"{"key":"K","display_name":"K","description":"","required":false,"kind":"generic"}"#,
        )
        .unwrap();
        assert!(plain.accepts("whatever"));
        assert_eq!(plain.default_value(), None);
        assert!(
            !serde_json::to_string(&plain).unwrap().contains("options"),
            "a field with no options keeps its JSON as it was"
        );
    }
}
