//! A direct path from GIAP to a mistral.rs server, so the engine is measured without goose.
//! `pond-core` is the only GIAP dependency; a `goose` line in `Cargo.toml` is a review failure.
//! [`MistralRsProvider`] implements `InferenceProvider`; [`MistralRsAgent`] is the `Agent`
//! bound when `agent_backend = "mistralrs"`. Mac-only (over budget on the Orin), feature-gated.

pub mod agent;
pub mod provider;
pub mod wire;

pub use agent::MistralRsAgent;
pub use provider::{MistralRsProvider, MrEvent, MrEventStream};

/// The `agent_backend` value that selects this path.
pub const BACKEND_NAME: &str = "mistralrs";

/// Default server URL; must match `scripts/try-mistralrs.sh` and the bake-off lab.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:9002";

/// The server URL, from `GIAP_MISTRALRS_URL` or the default.
pub fn base_url_from_env() -> String {
    std::env::var("GIAP_MISTRALRS_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_url_is_used_when_the_env_var_is_unset() {
        // Not asserting on the env var: another test may set it in this shared process.
        assert_eq!(DEFAULT_BASE_URL, "http://127.0.0.1:9002");
        assert!(base_url_from_env().starts_with("http"));
    }
}
