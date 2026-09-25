use serde::{Deserialize, Serialize};

/// An OAuth 2.1 Authorization Code + PKCE provider (e.g. Spotify). A user Client ID override
/// lives in the secret store as `{PROVIDER_ID}_CLIENT_ID` (uppercase).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthProviderConfig {
    /// Provider identifier (e.g. "spotify").
    pub id: String,
    /// Human-readable display name (e.g. "Spotify").
    pub display_name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub bundled_client_id: String,
    /// Secret key name under which the access token is stored.
    pub token_key: String,
    /// Secret key name under which the refresh token is stored.
    pub refresh_key: String,
}
