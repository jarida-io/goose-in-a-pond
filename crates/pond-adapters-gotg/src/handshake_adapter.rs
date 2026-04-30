//! GOTG Handshake Adapter
//!
//! GOTG-specific policy wrapper around any [`Handshake`] implementation
//! (in production, the SQLite-backed one in `pond-infra`). The wrapper
//! enforces GOTG-specific rules:
//!
//! - reject `client_type != "gotg"` for two-phase pairing endpoints
//! - log structured pairing events for the dashboard
//!
//! All token persistence and crypto live in the inner adapter.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::ports::handshake::{
    ChallengeResponse, Handshake, HandshakeRequest, HandshakeResponse, InitRequest, PairingCode,
    RefreshRequest, VerifyRequest,
};
use std::sync::Arc;

/// Wraps an inner `Handshake` adapter (typically `SqliteHandshakeAdapter`)
/// with GOTG-specific policy.
pub struct GotgHandshakeAdapter {
    inner: Arc<dyn Handshake>,
}

impl GotgHandshakeAdapter {
    pub fn new(inner: Arc<dyn Handshake>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Handshake for GotgHandshakeAdapter {
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse> {
        if request.client_type != "gotg" {
            tracing::warn!(
                client_type = %request.client_type,
                "rejecting non-gotg handshake on GOTG adapter"
            );
            return Ok(HandshakeResponse {
                accepted: false,
                session_token: None,
                refresh_token: None,
                expires_at: None,
                hostname: hostname::get()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "localhost".to_string()),
                server_version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: vec![],
                rejection_reason: Some("unsupported_client_type".to_string()),
            });
        }
        tracing::info!(client = %request.client_id, "gotg handshake (legacy)");
        self.inner.handshake(request).await
    }

    async fn validate_token(&self, token: &str) -> Result<bool> {
        self.inner.validate_token(token).await
    }

    async fn revoke_token(&self, token: &str) -> Result<()> {
        self.inner.revoke_token(token).await
    }

    async fn init_handshake(&self, request: InitRequest) -> Result<ChallengeResponse> {
        if request.client_type != "gotg" {
            anyhow::bail!("unsupported_client_type");
        }
        tracing::info!(client = %request.client_id, "gotg handshake init");
        self.inner.init_handshake(request).await
    }

    async fn verify_handshake(&self, request: VerifyRequest) -> Result<HandshakeResponse> {
        let resp = self.inner.verify_handshake(request).await?;
        if resp.accepted {
            tracing::info!("gotg handshake verify ok");
        } else {
            tracing::warn!(reason = ?resp.rejection_reason, "gotg handshake verify rejected");
        }
        Ok(resp)
    }

    async fn refresh(&self, request: RefreshRequest) -> Result<HandshakeResponse> {
        self.inner.refresh(request).await
    }

    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        self.inner.issue_pairing_code().await
    }

    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        self.inner.current_pairing_code().await
    }
}
