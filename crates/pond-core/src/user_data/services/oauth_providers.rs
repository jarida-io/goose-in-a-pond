//! Built-in OAuth PKCE providers; `{PROVIDER_ID}_CLIENT_ID` secrets override the client id.

use crate::security::domain::oauth_provider::OAuthProviderConfig;

pub fn builtin_oauth_providers() -> Vec<OAuthProviderConfig> {
    vec![OAuthProviderConfig {
        id: "spotify".to_string(),
        display_name: "Spotify".to_string(),
        authorize_url: "https://accounts.spotify.com/authorize".to_string(),
        token_url: "https://accounts.spotify.com/api/token".to_string(),
        scopes: vec![
            "user-read-playback-state".to_string(),
            "user-modify-playback-state".to_string(),
            "user-read-currently-playing".to_string(),
            "playlist-read-private".to_string(),
            "playlist-modify-public".to_string(),
            "playlist-modify-private".to_string(),
            // Read-only: Spotify 403s PUT/DELETE /me/tracks here even with user-library-modify.
            // New scopes only reach tokens issued after the user signs in again.
            "user-library-read".to_string(),
            "user-top-read".to_string(),
            "user-read-recently-played".to_string(),
        ],
        bundled_client_id: "9aa8d81a57624c87b392bf242c872224".to_string(),
        token_key: "SPOTIFY_ACCESS_TOKEN".to_string(),
        refresh_key: "SPOTIFY_REFRESH_TOKEN".to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_providers_contains_spotify() {
        let providers = builtin_oauth_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "spotify");
        assert_eq!(providers[0].display_name, "Spotify");
        assert!(!providers[0].scopes.is_empty());
        assert_eq!(providers[0].token_key, "SPOTIFY_ACCESS_TOKEN");
        assert_eq!(providers[0].refresh_key, "SPOTIFY_REFRESH_TOKEN");
    }

    #[test]
    fn provider_config_is_serializable() {
        let providers = builtin_oauth_providers();
        let json = serde_json::to_string(&providers[0]).unwrap();
        assert!(json.contains("spotify"));
        let roundtrip: OAuthProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.id, "spotify");
    }
}
