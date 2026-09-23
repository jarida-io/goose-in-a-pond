//! Driven Port: Handshake
//!
//! Device authentication and pairing between GIAP (server) and connecting
//! clients (the GOTG mobile app, other pond instances, the CLI, …).
//!
//! # Two-phase pairing protocol
//!
//! Pairing proves that the client holds a short-lived **pairing code** that the
//! operator read off this server's CLI/dashboard, without ever sending the code
//! over the wire:
//!
//! 1. `init_handshake(InitRequest) -> ChallengeResponse`
//!    The server mints a random 32-byte challenge bound to `client_id`,
//!    persists it with a short TTL, and returns it (base64) to the client.
//! 2. `verify_handshake(VerifyRequest)`
//!    The client computes a MAC over the challenge, keyed by the pairing code,
//!    and submits it. On success the server consumes the challenge + pairing
//!    code, registers the device, and mints a session+refresh token pair.
//!
//! # Channel binding
//!
//! A client that reached this server over pinned TLS names the key it pinned to
//! in [`VerifyRequest::channel_binding`] and folds it into the MAC. The server
//! recomputes with **its own** key, so the two agree only when the client is
//! talking to this server directly:
//!
//! ```text
//! bound   mac = HMAC(code, "goose-pair-client-v1\0" || challenge || \0 || client_id || \0 || spki)
//! unbound mac = HMAC(code, challenge || client_id)
//! ```
//!
//! and the server answers with [`HandshakeResponse::server_proof`] over the same
//! transcript under `goose-pair-server-v1`, which only something holding the
//! pairing code can produce.
//!
//! What this buys: the pin no longer has to be carried to the phone by a
//! trustworthy route. Somebody who intercepts the connection and presents their
//! own certificate -- by answering an mDNS query, say -- gets a client that
//! MACs over *their* key. Relaying that to this server fails the recomputation;
//! stripping the binding and relaying leaves a MAC over a transcript this
//! server no longer computes; and answering the client themselves fails the
//! server proof. A wrong pin therefore ends pairing in a visible failure
//! instead of a successful pair with the wrong pond.
//!
//! The binding is optional because one real caller has no channel to bind: the
//! desktop dashboard pairs over loopback HTTP, where there is no certificate
//! and no interceptor. Optional does not mean downgradable -- a client that
//! binds always binds, and nobody in the middle can compute the unbound MAC
//! either, because both forms need the pairing code.
//!
//! `refresh` rotates an expiring session token; `revoke_token` disconnects a
//! client. Pairing codes are issued by the server via `issue_pairing_code`
//! (shown on the CLI/dashboard) and are single-use.
//!
//! The legacy single-shot `handshake()` method is retained for the in-memory
//! `MockHandshake` (tests) and for already-paired clients that present a
//! pairing code directly.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request from a client wanting to connect to GIAP (legacy single-shot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Client identifier (e.g. mobile device UUID).
    pub client_id: String,
    /// Client type: "gotg", "pond", "cli", etc.
    pub client_type: String,
    /// Client version string.
    pub client_version: String,
    /// Optional pairing code (single-shot path).
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a handshake / refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Whether the handshake succeeded.
    pub accepted: bool,
    /// Session token for subsequent API calls (`Authorization: Bearer …`).
    pub session_token: Option<String>,
    /// Refresh token — populated by the two-phase / refresh paths only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC3339 expiry of the session token. Absent for legacy responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// GIAP hostname.
    pub hostname: String,
    /// GIAP version.
    pub server_version: String,
    /// Capabilities this GIAP instance supports.
    pub capabilities: Vec<String>,
    /// Reason if rejected.
    pub rejection_reason: Option<String>,
    /// Hex `HMAC-SHA256` proving this server holds the pairing code and serves
    /// the certificate the client pinned. See the channel-binding note above.
    ///
    /// Present exactly when the accepted request carried a
    /// [`VerifyRequest::channel_binding`]. A client that sent one and did not
    /// get one back is not talking to the pond it thinks it is, and must treat
    /// the pair as failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_proof: Option<String>,
}

/// Phase 1 request: the client asks for a challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub client_id: String,
    pub client_type: String,
    pub client_version: String,
}

/// Phase 1 response: the challenge the client must MAC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge_id: String,
    /// Base64-encoded random challenge bytes.
    pub challenge: String,
    /// RFC3339 expiry of the challenge.
    pub expires_at: String,
}

/// Phase 2 request: the client proves possession of the pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub challenge_id: String,
    /// Hex-encoded MAC over the transcript named by `channel_binding`.
    pub mac: String,
    /// Optional friendly device name to record in the devices table.
    #[serde(default)]
    pub device_name: Option<String>,
    /// The server public-key pin this client pinned its connection to, in
    /// `sha256/<base64>` form, when it reached the server over pinned TLS.
    ///
    /// `None` means there was no channel to bind -- the loopback dashboard --
    /// and selects the unbound MAC. Anything else must equal this server's own
    /// pin, or the request is rejected: see the channel-binding note above.
    #[serde(default)]
    pub channel_binding: Option<String>,
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
    /// The household member the device pairing with this code becomes.
    ///
    /// `None` -- the default and the only value any shipped caller produces
    /// today -- pairs an **unattributed** device: registered and usable, and
    /// not any member's phone. See [`Handshake::issue_pairing_code_for`] for
    /// why the member is captured here rather than in [`VerifyRequest`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// Who a valid session token was issued to.
///
/// One `session_tokens` row narrowed to the two identifiers a request can be
/// attributed with. They are **not** interchangeable, and the difference is the
/// whole of PAI-1's strongest rung:
///
/// * `client_id` is what the client called itself at `init_handshake`. It is
///   self-reported and it names an installation, not a person. It is what the
///   audit log records.
/// * `device_id` is the id of the `devices` row the pair wrote, and that row is
///   the one carrying `profile_id` (migration 0043) -- captured at pairing-CODE
///   issuance, on the host, so no client can name its own member.
///
/// They hold the same string today, because
/// `SqliteHandshakeAdapter::verify_handshake` derives the device id from the
/// client id. They are separate fields anyway: the day a device id stops being
/// the client's own word for itself, the identity rung must follow the column
/// with the foreign key on it and not the other one.
///
/// `device_id` is not `Option`. `session_tokens.device_id` is `NOT NULL`, so a
/// token that exists has one; "I do not know which device" is expressed by the
/// `Option<TokenCaller>` the lookup returns, and there is deliberately no
/// second way to say it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCaller {
    /// The client id the token was issued to.
    pub client_id: String,
    /// The `devices` row this token's pairing registered.
    pub device_id: String,
}

/// Driven Port: device authentication and pairing.
///
/// New two-phase methods carry default impls that return "unsupported" so
/// lightweight adapters (e.g. `MockHandshake`) only need the three core
/// methods. The SQLite adapter overrides them all.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Legacy single-shot handshake (client presents a pairing code directly,
    /// or none for the mock). Real clients should prefer `init`/`verify`.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// The caller a valid token was issued to — client **and** device — if this
    /// adapter can say.
    ///
    /// [`validate_token`](Self::validate_token) answers only yes or no, so a
    /// caller that has authenticated a request still cannot name who made it —
    /// which is why `Principal::token(..)` had nothing to populate it with and
    /// no `Principal` was ever constructed in production.
    ///
    /// # Why the device rides here and cannot ride anywhere else
    ///
    /// `IdentificationSource::PairedDevice` is the **strongest** rung of
    /// [`identity_resolution::resolve`], outranking both face and explicit
    /// identification. The device id that feeds it must therefore come from the
    /// token **the pond itself issued** — never from a header, a body field or
    /// a query parameter, all of which a client controls and any of which would
    /// outrank every proof the pond can actually make. This method is the only
    /// door that answer comes through.
    ///
    /// [`identity_resolution::resolve`]: crate::user_data::services::identity_resolution::resolve
    ///
    /// # Why this is defaulted, and what stops the default being the answer
    ///
    /// A default on a trait that answers a security question is normally a
    /// decision made by whoever forgot to override it — PAI-2 P7's words, and
    /// the reason every method on
    /// [`DeviceAttribution`](crate::user_data::ports::device_attribution::DeviceAttribution)
    /// is required. Two things make the default the right call *here* and
    /// neither is "it was less work":
    ///
    /// 1. **The default narrows.** `Ok(None)` means no device on the request,
    ///    so the paired-device rung is skipped and resolution falls through to
    ///    explicit, then face, then guest. A forgotten override loses a
    ///    capability; it cannot grant one. That is the opposite of the usual
    ///    defaulted-method hazard, where the omission is what widens access.
    /// 2. **The forgetting is caught anyway.** The inert-feature risk is real —
    ///    this programme has shipped three phases that were inert — so it is
    ///    guarded twice rather than argued away:
    ///    `crates/pond-infra/tests/device_rung_wiring.rs` walks every
    ///    `impl Handshake for` in the workspace and fails on one that answers
    ///    neither this method nor the allowlist, and
    ///    [`client_id_for_token`](Self::client_id_for_token) below is derived
    ///    from this method, so an adapter that loses the override also loses
    ///    the client id it has been answering since #93.
    async fn caller_for_token(&self, _token: &str) -> Result<Option<TokenCaller>> {
        Ok(None)
    }

    /// The `client_id` a valid token was issued to, if this adapter can say.
    ///
    /// Derived from [`caller_for_token`](Self::caller_for_token) rather than
    /// implemented beside it, exactly as
    /// [`issue_pairing_code`](Self::issue_pairing_code) is derived from
    /// [`issue_pairing_code_for`](Self::issue_pairing_code_for): an adapter
    /// cannot support one and not the other, and two lookups over the same row
    /// cannot drift into disagreeing about which client a token belongs to.
    ///
    /// "I do not know" is a truthful answer, and an audit entry saying
    /// `token:<unknown>` is better than one naming a client id that was
    /// inferred.
    async fn client_id_for_token(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .caller_for_token(token)
            .await?
            .map(|caller| caller.client_id))
    }

    /// Revoke every session a device holds, and report how many were live.
    ///
    /// Removing a device from the household registry has to take its access
    /// with it. `session_tokens.device_id` carries no foreign key, and nothing
    /// cascades onto that table, so deleting the `devices` row on its own left
    /// the tokens valid -- an operator who removed a lost phone from the device
    /// list would have been told it was gone while it carried on working.
    ///
    /// # Why this has no default
    ///
    /// Every other new method on this trait is defaulted, and each of those
    /// defaults **narrows**: a forgotten override loses a capability. A default
    /// here would do the opposite. `Ok(0)` would mean "revoked nothing", the
    /// caller would delete the device row anyway, and the omission would widen
    /// access while reading like success. So it is required, and an adapter
    /// that cannot revoke has to say so out loud.
    async fn revoke_device(&self, device_id: &str) -> Result<u64>;

    /// Revoke a session token (disconnect a client).
    async fn revoke_token(&self, token: &str) -> Result<()>;

    /// Phase 1: issue a challenge bound to a `client_id`.
    async fn init_handshake(&self, _request: InitRequest) -> Result<ChallengeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Phase 2: verify the client's MAC and mint tokens.
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Exchange a refresh token for a fresh session+refresh pair.
    async fn refresh(&self, _request: RefreshRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!("refresh not supported by this adapter"))
    }

    /// Issue a single-use pairing code **bound to a household member**, for the
    /// operator to read aloud / type into a client. Returns the plaintext code
    /// (the only place it is visible).
    ///
    /// `profile_id: None` issues an ordinary unattributed code, which is what
    /// [`issue_pairing_code`](Self::issue_pairing_code) does and what every
    /// shipped caller does today.
    ///
    /// # Why the member is captured here and not in [`VerifyRequest`]
    ///
    /// The obvious alternative is for the pairing client to say who it is. That
    /// is the same shape as the live hole PAI-1 P4 closed on
    /// `PUT /sessions/{id}/user`, which took a `profile_id` from the request
    /// body and bound it at `Explicit` strength with no ownership check at all.
    /// It would be worse here: `IdentificationSource::PairedDevice` is the
    /// **strongest** rung of `identity_resolution::resolve` and outranks both
    /// face and explicit, so a client-asserted profile would not merely be
    /// unproven -- it would outrank every proof the pond can actually make.
    ///
    /// A pairing code, by contrast, is minted on the host: both
    /// `handshake_pairing_code` and `handshake_issue_pairing_code` refuse a
    /// non-loopback peer inside the handler. Binding the member at issuance
    /// means the answer to "whose device is this?" comes from somebody standing
    /// at the pond, and the pairing client cannot influence it.
    async fn issue_pairing_code_for(&self, _profile_id: Option<&str>) -> Result<PairingCode> {
        Err(anyhow::anyhow!(
            "pairing-code issuance not supported by this adapter"
        ))
    }

    /// Issue an unattributed single-use pairing code.
    ///
    /// Kept as the zero-argument form because it is what the loopback issuance
    /// route and the startup banner call, and an unattributed pair is the right
    /// default: pairing usually happens before anyone has said who they are.
    /// Adapters implement [`issue_pairing_code_for`](Self::issue_pairing_code_for);
    /// this delegates, so an adapter cannot support one and not the other.
    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        self.issue_pairing_code_for(None).await
    }

    /// The most recently issued, unexpired, unconsumed pairing code, if any.
    /// Used by the loopback dashboard endpoint to re-display the code.
    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        Ok(None)
    }
}
