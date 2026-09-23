use anyhow::Result;
use async_trait::async_trait;
use pond_core::security::ports::handshake::{Handshake, HandshakeRequest, HandshakeResponse};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mock implementation of Handshake for testing and development.
///
/// Stores session tokens in memory and validates them.
/// In production, this would be replaced with a real authentication system.
pub struct MockHandshake {
    /// Session tokens mapped to their validity
    valid_tokens: Arc<RwLock<HashMap<String, bool>>>,
}

impl MockHandshake {
    pub fn new() -> Self {
        Self {
            valid_tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add a valid token for testing
    pub async fn add_valid_token(&self, token: String) {
        self.valid_tokens.write().await.insert(token, true);
    }

    /// Revoke a token for testing
    pub async fn revoke_token_for_testing(&self, token: &str) {
        self.valid_tokens
            .write()
            .await
            .insert(token.to_string(), false);
    }
}

impl Default for MockHandshake {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Handshake for MockHandshake {
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse> {
        // Generate a simple token: client_id:timestamp
        let token = format!(
            "{}:{}",
            request.client_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );

        // Store as valid
        self.valid_tokens.write().await.insert(token.clone(), true);

        Ok(HandshakeResponse {
            accepted: true,
            session_token: Some(token),
            refresh_token: None,
            expires_at: None,
            hostname: hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "localhost".to_string()),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![
                "chat".to_string(),
                "devices".to_string(),
                "settings".to_string(),
            ],
            rejection_reason: None,
            // The mock has no TLS identity and accepts no binding, so it has
            // nothing to prove.
            server_proof: None,
        })
    }

    async fn validate_token(&self, token: &str) -> Result<bool> {
        let tokens = self.valid_tokens.read().await;
        Ok(tokens.get(token).copied().unwrap_or(false))
    }

    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        // The mock issues tokens without recording a device, so it has none to
        // revoke and says so rather than reporting a number it cannot back up.
        Ok(0)
    }

    async fn revoke_token(&self, token: &str) -> Result<()> {
        self.valid_tokens.write().await.remove(token);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handshake_creates_token() {
        let hs = MockHandshake::new();
        let request = HandshakeRequest {
            client_id: "test-client".to_string(),
            client_type: "gotg".to_string(),
            client_version: "1.0.0".to_string(),
            pairing_code: None,
        };

        let response = hs.handshake(request).await.unwrap();
        assert!(response.accepted);
        assert!(response.session_token.is_some());
    }

    #[tokio::test]
    async fn test_validate_token_after_handshake() {
        let hs = MockHandshake::new();
        let request = HandshakeRequest {
            client_id: "test-client".to_string(),
            client_type: "gotg".to_string(),
            client_version: "1.0.0".to_string(),
            pairing_code: None,
        };

        let response = hs.handshake(request).await.unwrap();
        let token = response.session_token.unwrap();

        // Token should be valid after handshake
        let is_valid = hs.validate_token(&token).await.unwrap();
        assert!(is_valid);
    }

    #[tokio::test]
    async fn test_validate_invalid_token() {
        let hs = MockHandshake::new();
        let is_valid = hs.validate_token("invalid-token").await.unwrap();
        assert!(!is_valid);
    }

    #[tokio::test]
    async fn test_revoke_token() {
        let hs = MockHandshake::new();
        let request = HandshakeRequest {
            client_id: "test-client".to_string(),
            client_type: "gotg".to_string(),
            client_version: "1.0.0".to_string(),
            pairing_code: None,
        };

        let response = hs.handshake(request).await.unwrap();
        let token = response.session_token.unwrap();

        // Token should be valid
        assert!(hs.validate_token(&token).await.unwrap());

        // Revoke it
        hs.revoke_token(&token).await.unwrap();

        // Token should no longer be valid
        assert!(!hs.validate_token(&token).await.unwrap());
    }
}
