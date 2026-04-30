//! Driven Port: Handshake
//!
//! Defines the contract for device authentication between GIAP and
//! connecting clients (GOTG mobile app, other pond instances, etc.)
//!
//! # Two-phase pairing protocol
//!
//! 1. `init_handshake(InitRequest) -> ChallengeResponse`
//!    Server generates a 32-byte challenge bound to the client_id, persists
//!    it with a short TTL, and returns it (base64) to the client.
//! 2. `verify_handshake(VerifyRequest) -> HandshakeResponse`
//!    Client computes `mac = HMAC-SHA256(pairing_code, challenge || client_id)`
//!    and submits it. On success the server consumes the challenge + pairing
//!    code, mints a session+refresh token pair, and registers the device.
//!
//! Refresh and revoke complete the lifecycle. Pairing codes are issued by the
//! server (CLI/dashboard) via `issue_pairing_code` and are single-use.
//!
//! The legacy single-shot `handshake()` method is preserved for the in-memory
//! `MockHandshake` used by tests and for backwards-compatibility with older
//! clients that send `pairing_code` directly.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request from a client wanting to connect to GIAP (legacy single-shot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Client identifier (e.g. mobile device UUID)
    pub client_id: String,
    /// Client type: "gotg", "pond", "cli", etc.
    pub client_type: String,
    /// Client version string
    pub client_version: String,
    /// Optional pre-shared key or pairing code
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a successful handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub accepted: bool,
    pub session_token: Option<String>,
    /// Refresh token — only populated by the two-phase protocol.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub refresh_token: Option<String>,
    /// RFC3339 expiry of the session token. Empty for legacy responses.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<String>,
    pub hostname: String,
    pub server_version: String,
    pub capabilities: Vec<String>,
    pub rejection_reason: Option<String>,
}

/// Init phase: client asks for a challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub client_id: String,
    pub client_type: String,
    pub client_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge_id: String,
    /// Base64-encoded random challenge bytes.
    pub challenge: String,
    pub expires_at: String,
}

/// Verify phase: client proves possession of the pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub challenge_id: String,
    /// Hex-encoded HMAC-SHA256(pairing_code, challenge || client_id).
    pub mac: String,
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Server-issued pairing code shown on CLI/dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    /// 6-digit code shown to the user.
    pub code: String,
    pub expires_at: String,
}

/// Driven Port: device authentication and pairing.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Legacy single-shot handshake. Real adapters should prefer the two-phase
    /// `init_handshake` / `verify_handshake` flow; this method is retained for
    /// `MockHandshake` and for already-paired clients reusing a token.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// Revoke a session token (disconnect a client).
    async fn revoke_token(&self, token: &str) -> Result<()>;

    /// Phase 1: issue a challenge bound to a client_id.
    ///
    /// Default impl returns "not implemented"; the SQLite adapter overrides.
    async fn init_handshake(&self, _request: InitRequest) -> Result<ChallengeResponse> {
        Err(anyhow::anyhow!("two-phase handshake not supported by this adapter"))
    }

    /// Phase 2: verify the client's MAC and mint tokens.
    async fn verify_handshake(
        &self,
        _request: VerifyRequest,
    ) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!("two-phase handshake not supported by this adapter"))
    }

    /// Exchange a refresh token for a fresh session+refresh pair.
    async fn refresh(&self, _request: RefreshRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!("refresh not supported by this adapter"))
    }

    /// Issue a single-use pairing code for the operator to read aloud / type
    /// into a client. Returns the plaintext code (only place it is visible).
    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        Err(anyhow::anyhow!("pairing-code issuance not supported by this adapter"))
    }

    /// Returns the most recently issued, unexpired, unconsumed pairing code,
    /// if any. Used by the loopback dashboard endpoint.
    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        Ok(None)
    }
}
