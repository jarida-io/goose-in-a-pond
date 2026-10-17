//! Driven port: device pairing and session tokens.
//!
//! # Phones pair with CPace (2026-10-13)
//!
//! A phone proves it holds the single-use pairing code with CPace (`draft-irtf-cfrg-cpace-21`,
//! suite `CPACE-RISTR255-SHA512`), in two round trips with the Pond as initiator:
//!
//! ```text
//! init    phone -> {client_id, client_type, client_version, channel_binding, client_nonce}
//!         pond  -> {challenge_id, challenge, expires_at, suite, share: Ya}
//! verify  phone -> {challenge_id, device_name?, share: Yb, confirm: T_client}
//!         pond  -> HandshakeResponse with server_proof: T_server
//! ```
//!
//! The code is the password, and the channel identifier carries the Pond's TLS key pin, so a
//! phone that pinned an interceptor's key derives a different generator and fails. Nothing on
//! the wire can be tested against a code offline; each guess is one live `verify`.
//!
//! The desktop dashboard pairs over loopback with the code it was just handed, using the
//! original `HMAC-SHA256(code, challenge || client_id)` exchange ([`VerifyRequest`]). The routes
//! refuse that exchange from any peer but loopback.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::network::domain::claim::RetryAfter;
use crate::network::domain::credential::Credential;

/// Request from a client wanting to connect to GIAP (legacy single-shot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Client identifier (e.g. mobile device UUID).
    pub client_id: String,
    /// Client type: "gotg", "pond", "cli", etc.
    pub client_type: String,
    pub client_version: String,
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a handshake / refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub accepted: bool,
    /// Session token for subsequent API calls (`Authorization: Bearer …`).
    pub session_token: Option<String>,
    /// Refresh token — populated by the two-phase / refresh paths only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC3339 expiry of the session token. Absent for legacy responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub hostname: String,
    pub server_version: String,
    pub capabilities: Vec<String>,
    pub rejection_reason: Option<String>,
    /// The Pond's CPace confirmation tag `T_server`, lowercase hex (64 bytes). Present on an
    /// accepted [`PakeVerifyRequest`] only; a phone that cannot verify it must discard the tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_proof: Option<String>,
}

/// Phase 1 of loopback pairing: the dashboard asks for a challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub client_id: String,
    pub client_type: String,
    pub client_version: String,
}

/// Phase 1 response of loopback pairing: the challenge the dashboard must MAC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge_id: String,
    /// Base64-encoded random challenge bytes.
    pub challenge: String,
    /// RFC3339 expiry of the challenge.
    pub expires_at: String,
}

/// Phase 2 of loopback pairing: `mac = HMAC-SHA256(code, challenge || client_id)`, hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub challenge_id: String,
    /// Hex-encoded MAC over `challenge || client_id`.
    pub mac: String,
    /// Optional friendly device name to record in the devices table.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// CPace phase 1: a phone asks the Pond to commit to the current pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakeInitRequest {
    pub client_id: String,
    pub client_type: String,
    pub client_version: String,
    /// The `sha256/<base64>` key pin the phone's TLS is pinned to. A diagnostic only: the
    /// Pond always runs CPace under its own pin, so a wrong pin fails either way, but naming it
    /// here fails early and clearly with `channel_binding_mismatch`.
    pub channel_binding: String,
    /// 32 random bytes from the phone, standard base64: the second half of the session id.
    pub client_nonce: String,
    /// The secret the phone holds: the card's setup code, a pairing code, or (on the setup
    /// channel only) its own refresh token.
    #[serde(default)]
    pub credential: Credential,
}

/// CPace phase 1 response: the Pond's half of the session id and its public share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakeChallenge {
    pub challenge_id: String,
    /// 32 random bytes, standard base64: the first half of the session id.
    pub challenge: String,
    /// RFC3339 expiry of the challenge.
    pub expires_at: String,
    /// Always `CPACE-RISTR255-SHA512`.
    pub suite: String,
    /// `Ya`, the 32-byte ristretto255 encoding, standard base64.
    pub share: String,
}

/// Why the Pond would not start a CPace run. Each maps to one stable error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PakeRefusal {
    /// The phone named a key pin that is not this Pond's, or this Pond has none.
    ChannelBindingMismatch,
    /// No unexpired, unconsumed pairing code to commit to.
    NoActivePairingCode,
    /// `client_nonce` is not 32 bytes of standard base64.
    InvalidClientNonce,
    /// Too many runs are waiting for their `verify`; try again shortly.
    Busy,
    /// The pond is claimed or has no setup code; both read the same to a phone on purpose.
    SetupCodeUnavailable,
    /// Too many wrong setup codes; nothing is checked until `retry_after` has passed.
    SetupCodeCooling { retry_after: RetryAfter },
    /// `credential: "device"` over HTTPS, which has `/handshake/refresh` for that.
    InvalidCredential,
    /// Too many failed device runs for this `client_id`; none starts until `retry_after`.
    DeviceCooling { retry_after: RetryAfter },
}

impl PakeRefusal {
    /// The stable code clients branch on.
    pub fn code(self) -> &'static str {
        match self {
            Self::ChannelBindingMismatch => "channel_binding_mismatch",
            Self::NoActivePairingCode => "no_active_pairing_code",
            Self::InvalidClientNonce => "invalid_client_nonce",
            Self::Busy => "pairing_busy",
            Self::SetupCodeUnavailable => "setup_code_unavailable",
            Self::SetupCodeCooling { .. } => "setup_code_cooling",
            Self::InvalidCredential => "invalid_credential",
            Self::DeviceCooling { .. } => "device_cooling",
        }
    }

    pub fn retry_after(self) -> Option<RetryAfter> {
        match self {
            Self::SetupCodeCooling { retry_after } | Self::DeviceCooling { retry_after } => {
                Some(retry_after)
            }
            _ => None,
        }
    }
}

/// CPace phase 2: the phone's share and its confirmation tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PakeVerifyRequest {
    pub challenge_id: String,
    /// `Yb`, the 32-byte ristretto255 encoding, standard base64.
    pub share: String,
    /// `T_client`, the 64-byte HMAC-SHA-512 tag, lowercase hex.
    pub confirm: String,
    /// Optional friendly device name to record in the devices table.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// Length of each setup-channel key (ChaCha20-Poly1305).
pub const CHANNEL_KEY_LEN: usize = 32;

/// The setup channel's per-direction keys from one accepted CPace run: its ISK stays in the
/// adapter and only these leave, for one session. Wiped on drop; `Debug` redacts.
pub struct ChannelKeys {
    pond_to_phone: [u8; CHANNEL_KEY_LEN],
    phone_to_pond: [u8; CHANNEL_KEY_LEN],
}

impl ChannelKeys {
    pub fn new(
        pond_to_phone: &[u8; CHANNEL_KEY_LEN],
        phone_to_pond: &[u8; CHANNEL_KEY_LEN],
    ) -> Self {
        Self {
            pond_to_phone: *pond_to_phone,
            phone_to_pond: *phone_to_pond,
        }
    }

    /// `k_p2c`: seals what the pond sends.
    pub fn pond_to_phone(&self) -> &[u8; CHANNEL_KEY_LEN] {
        &self.pond_to_phone
    }

    /// `k_c2p`: seals what the phone sends.
    pub fn phone_to_pond(&self) -> &[u8; CHANNEL_KEY_LEN] {
        &self.phone_to_pond
    }
}

impl Drop for ChannelKeys {
    fn drop(&mut self) {
        crate::shared::domain::wipe::wipe(&mut self.pond_to_phone);
        crate::shared::domain::wipe::wipe(&mut self.phone_to_pond);
    }
}

impl std::fmt::Debug for ChannelKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelKeys(redacted)")
    }
}

/// What a setup-channel `verify` produced: the answer HTTPS would give, and, only for an accepted
/// run, the keys that seal the rest of the session.
#[derive(Debug)]
pub struct ChannelVerify {
    pub response: HandshakeResponse,
    pub keys: Option<ChannelKeys>,
}

/// Exchange a refresh token for a fresh session+refresh pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// A server-issued pairing code, shown on the CLI/dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    /// 6-digit code shown to the operator (leading zeros preserved).
    pub code: String,
    /// RFC3339 expiry of the code.
    pub expires_at: String,
    /// Member the device paired with this code becomes; `None` pairs an unattributed device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// Who a valid session token was issued to.
/// The ids match today, but identity must follow `device_id`: `client_id` is self-reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCaller {
    pub client_id: String,
    /// The `devices` row this token's pairing registered.
    pub device_id: String,
}

/// Driven port: device authentication and pairing.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Legacy single-shot handshake; real clients should prefer `init`/`verify`.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// The client and device a valid token was issued to, if this adapter can say.
    /// Sole source of the paired-device identity rung, which outranks face and explicit, so the
    /// device id must never come from request input. The `Ok(None)` default only skips that rung.
    async fn caller_for_token(&self, _token: &str) -> Result<Option<TokenCaller>> {
        Ok(None)
    }

    /// The `client_id` a valid token was issued to, if known; derived so the two cannot disagree.
    async fn client_id_for_token(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .caller_for_token(token)
            .await?
            .map(|caller| caller.client_id))
    }

    /// Revoke every session a device holds; returns how many were live. Required, not defaulted:
    /// no FK cascades from `devices`, so an `Ok(0)` default would leave removed devices working.
    async fn revoke_device(&self, device_id: &str) -> Result<u64>;

    /// Revoke a session token (disconnect a client).
    async fn revoke_token(&self, token: &str) -> Result<()>;

    /// Loopback phase 1: issue a challenge bound to a `client_id`.
    async fn init_handshake(&self, _request: InitRequest) -> Result<ChallengeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Loopback phase 2: verify the dashboard's MAC and mint tokens.
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// CPace phase 1: commit to the newest active pairing code and answer with `Ya`.
    async fn init_pake(
        &self,
        _request: PakeInitRequest,
    ) -> Result<std::result::Result<PakeChallenge, PakeRefusal>> {
        Err(anyhow::anyhow!(
            "CPace pairing not supported by this adapter"
        ))
    }

    /// CPace phase 2: check `Yb` and `T_client`, then consume the code and mint tokens. A
    /// refusal is `accepted: false` with a `rejection_reason`, never an `Err`.
    async fn verify_pake(&self, _request: PakeVerifyRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!(
            "CPace pairing not supported by this adapter"
        ))
    }

    /// As [`Self::init_pake`], for the in-process setup channel. The run is the same exchange
    /// under the same limits, and only [`Self::verify_channel_pake`] can finish it. Only here
    /// may a phone name [`Credential::Device`].
    async fn init_channel_pake(
        &self,
        _request: PakeInitRequest,
    ) -> Result<std::result::Result<PakeChallenge, PakeRefusal>> {
        Err(anyhow::anyhow!(
            "the setup channel is not supported by this adapter"
        ))
    }

    /// As [`Self::verify_pake`], plus the keys for an accepted run. No HTTP route calls this, so
    /// only an in-process transport can hold them; an HTTPS run is unknown here, and the reverse.
    async fn verify_channel_pake(&self, _request: PakeVerifyRequest) -> Result<ChannelVerify> {
        Err(anyhow::anyhow!(
            "the setup channel is not supported by this adapter"
        ))
    }

    /// Which secret a waiting run was started with, so a verify can be audited under the right
    /// method without the phone saying it again. `None` once the run is spent or unknown.
    async fn pake_credential(&self, _challenge_id: &str) -> Option<Credential> {
        None
    }

    /// Exchange a refresh token for a fresh session+refresh pair.
    async fn refresh(&self, _request: RefreshRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!("refresh not supported by this adapter"))
    }

    /// Issue a single-use pairing code bound to a member; returns the only plaintext copy.
    /// Bound on the host, never by the pairing client: the paired-device rung outranks all proofs.
    /// Issuing retires every older active code, so the code on screen is the only one that works.
    async fn issue_pairing_code_for(&self, _profile_id: Option<&str>) -> Result<PairingCode> {
        Err(anyhow::anyhow!(
            "pairing-code issuance not supported by this adapter"
        ))
    }

    /// Issue an unattributed single-use pairing code; adapters implement `issue_pairing_code_for`.
    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        self.issue_pairing_code_for(None).await
    }

    /// The latest unexpired, unconsumed pairing code, if any, for the dashboard to re-display.
    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_device_refusals_carry_their_wire_codes() {
        assert_eq!(PakeRefusal::InvalidCredential.code(), "invalid_credential");
        assert_eq!(PakeRefusal::InvalidCredential.retry_after(), None);
        let cooling = PakeRefusal::DeviceCooling {
            retry_after: RetryAfter(Duration::from_secs(90)),
        };
        assert_eq!(cooling.code(), "device_cooling");
        assert_eq!(
            cooling.retry_after(),
            Some(RetryAfter(Duration::from_secs(90)))
        );
    }
}
