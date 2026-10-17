//! What an installed catalogue extension starts with. Key values live only in the secret store:
//! an installed extension's saved row keeps none, and each start reads them from the store, so
//! a deleted key stops working everywhere at once.

use std::collections::HashMap;

use crate::mcp::domain::marketplace::MarketplaceExtension;

/// `env` without any secret `ext` declares: what may be saved in the extension's row.
pub fn without_declared_secrets(
    env: &HashMap<String, String>,
    ext: &MarketplaceExtension,
) -> HashMap<String, String> {
    env.iter()
        .filter(|(key, _)| !ext.required_secrets.iter().any(|sr| &sr.key == *key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The blocking secrets `stored` lacks, by name; a blank value counts as missing.
pub fn missing_blocking_secrets(
    ext: &MarketplaceExtension,
    stored: &HashMap<String, String>,
) -> Vec<String> {
    ext.blocking_secrets()
        .filter(|sr| stored.get(&sr.key).is_none_or(|v| v.trim().is_empty()))
        .map(|sr| sr.key.clone())
        .collect()
}

/// A start prepared from an extension's saved row and the secret store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnv {
    /// The environment to start with, or the blocking secrets missing from the store.
    pub start: Result<HashMap<String, String>, Vec<String>>,
    /// The row's environment with every declared secret removed.
    pub row_env: HashMap<String, String>,
    /// Whether the row held a declared secret, so it must be saved again without it.
    pub row_changed: bool,
}

/// Prepare a start: the row's environment less every declared secret, then the stored value of
/// each secret the extension's environment takes. `stored` holds the store's values for the
/// extension's declared secrets.
pub fn prepare_extension_env(
    row_env: &HashMap<String, String>,
    ext: &MarketplaceExtension,
    stored: &HashMap<String, String>,
) -> PreparedEnv {
    let scrubbed = without_declared_secrets(row_env, ext);
    let row_changed = scrubbed.len() != row_env.len();
    let missing = missing_blocking_secrets(ext, stored);
    let start = if missing.is_empty() {
        let mut env = scrubbed.clone();
        for sr in ext.env_secrets() {
            if let Some(value) = stored.get(&sr.key) {
                env.insert(sr.key.clone(), value.clone());
            }
        }
        Ok(env)
    } else {
        Err(missing)
    };
    PreparedEnv {
        start,
        row_env: scrubbed,
        row_changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A required pasted key, an optional one, a host-only key and a sign-in.
    fn ext() -> MarketplaceExtension {
        serde_json::from_value(serde_json::json!({
            "id": "github",
            "name": "GitHub",
            "description": "",
            "kind": "stdio",
            "command": "npx",
            "category": "development",
            "author": "test",
            "required_secrets": [
                {"key": "TOKEN", "display_name": "Token", "description": "", "required": true,
                 "kind": "api_key"},
                {"key": "OPTIONAL", "display_name": "Optional", "description": "",
                 "required": false, "kind": "generic"},
                {"key": "SIGNING", "display_name": "Signing", "description": "", "required": false,
                 "kind": "api_key", "host_only": true},
                {"key": "SIGN_IN", "display_name": "Sign in", "description": "", "required": true,
                 "kind": "oauth_flow"}
            ]
        }))
        .unwrap()
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_saved_row_keeps_no_declared_secret() {
        let row = map(&[
            ("TOKEN", "old-value"),
            ("OPTIONAL", "o"),
            ("SIGNING", "s"),
            ("SIGN_IN", "t"),
            ("GIAP_SERVER_URL", "http://127.0.0.1:4000"),
        ]);
        assert_eq!(
            without_declared_secrets(&row, &ext()),
            map(&[("GIAP_SERVER_URL", "http://127.0.0.1:4000")])
        );
    }

    #[test]
    fn a_start_takes_the_stores_values_not_the_rows() {
        let row = map(&[("TOKEN", "old-value"), ("GIAP_SERVER_URL", "u")]);
        let stored = map(&[("TOKEN", "new-value"), ("SIGNING", "s"), ("OPTIONAL", "o")]);
        let prepared = prepare_extension_env(&row, &ext(), &stored);
        assert_eq!(
            prepared.start,
            Ok(map(&[
                ("TOKEN", "new-value"),
                ("OPTIONAL", "o"),
                ("GIAP_SERVER_URL", "u")
            ])),
            "a host-only secret never enters the environment"
        );
        assert_eq!(prepared.row_env, map(&[("GIAP_SERVER_URL", "u")]));
        assert!(prepared.row_changed, "the row's copy of the key must go");
    }

    #[test]
    fn a_deleted_key_is_not_rescued_by_the_rows_copy() {
        let row = map(&[("TOKEN", "old-value")]);
        let prepared = prepare_extension_env(&row, &ext(), &HashMap::new());
        assert_eq!(prepared.start, Err(vec!["TOKEN".to_string()]));
        assert!(prepared.row_env.is_empty());
        assert!(prepared.row_changed);
    }

    #[test]
    fn a_blank_stored_key_is_missing_and_an_optional_one_never_blocks() {
        let stored = map(&[("TOKEN", "  ")]);
        assert_eq!(
            missing_blocking_secrets(&ext(), &stored),
            vec!["TOKEN".to_string()]
        );
        let stored = map(&[("TOKEN", "a-value")]);
        assert!(
            missing_blocking_secrets(&ext(), &stored).is_empty(),
            "neither the optional key nor the sign-in blocks"
        );
    }

    #[test]
    fn a_clean_row_is_left_unchanged() {
        let row = map(&[("GIAP_SERVER_URL", "u")]);
        let prepared = prepare_extension_env(&row, &ext(), &map(&[("TOKEN", "a-value")]));
        assert!(!prepared.row_changed);
        assert!(prepared.start.is_ok());
    }
}
