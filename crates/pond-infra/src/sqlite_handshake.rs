//! SQLite-backed [`Handshake`] adapter: CPace pairing for phones, HMAC pairing for the
//! loopback dashboard, and session tokens.
//!
//! Only sha256 hashes of codes and tokens are stored; tokens are base64 of 32 random bytes.
//! MAC, tag and hash comparisons are constant-time. The pairing code never crosses the wire.
//!
//! Phones pair with CPace ([`crate::pairing_cpace`]). `init_pake` commits to the newest active
//! code and keeps the Pond's half of the run in memory, never on disk, until `verify_pake` burns
//! it. Each code allows [`PAKE_FAILURE_BUDGET`] confirmation attempts; the attempt that spends
//! the budget retires the code, so an attacker gets at most that many online guesses per code
//! whatever addresses it holds.
//!
//! A phone holding the pond's printed setup code runs the same exchange under its own channel
//! label. That code is guarded by [`SetupClaimService`] (a guess throttle, and a claim latch read
//! live) rather than by the pairing-code budget, and it never touches `pairing_codes`. Every
//! completed pairing, by either secret, latches the claim.
//!
//! The BLE setup channel runs the same exchange in process (`init_channel_pake`), and only its
//! accepted runs yield channel keys. A run is finished only on the transport that started it.
//!
//! There, and only there, a paired phone may resume with `credential: "device"`: the password is
//! its newest refresh hash, and a confirmed run rotates that session as `refresh` does.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use once_cell::sync::Lazy;
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use pond_core::network::domain::claim::ClaimRefusal;
use pond_core::network::domain::credential::Credential;
use pond_core::network::services::device_resume_throttle::DeviceResumeThrottle;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::security::ports::handshake::{
    ChallengeResponse, ChannelKeys, ChannelVerify, Handshake, HandshakeRequest, HandshakeResponse,
    InitRequest, PairingCode, PakeChallenge, PakeInitRequest, PakeRefusal, PakeVerifyRequest,
    RefreshRequest, TokenCaller, VerifyRequest,
};
use pond_core::user_data::ports::device_attribution::checked_profile_id;
use pond_core::user_data::services::member_attribution::sole_member;

use crate::pairing_cpace::{self, Confirmed, CpaceError, Initiator};

const PAIRING_CODE_TTL_MIN: i64 = 10;
const CHALLENGE_TTL_SEC: i64 = 60;
const SESSION_TTL_HOURS: i64 = 24;
const REFRESH_TTL_DAYS: i64 = 30;

/// CPace confirmation attempts one pairing code allows; the attempt that spends it retires the
/// code. Bounds online guessing at 10 in 10^6 per code, whatever addresses an attacker holds.
pub const PAKE_FAILURE_BUDGET: u32 = 10;
/// CPace runs that may wait for their `verify` at once. Bounds memory, not guessing: each run
/// is one challenge and the code's budget is what limits guesses.
const MAX_PENDING_PAKE: usize = 256;

type HmacSha256 = Hmac<Sha256>;

/// What a paired client may rely on. `routines`: device-action routines and asking before a
/// missed run, so the app shows those controls only to a pond that has them.
pub const POND_CAPABILITIES: &[&str] = &["chat", "devices", "settings", "routines"];

/// Process-local plaintext codes (only hashes persist), so a restart invalidates unused ones.
static ISSUED_CODE_CACHE: Lazy<RwLock<HashMap<String, String>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub struct SqliteHandshakeAdapter {
    pool: Pool<Sqlite>,
    hostname: String,
    server_version: String,
    capabilities: Vec<String>,
    spki_pin: Option<String>,
    pake: Mutex<PakeState>,
    /// `None` refuses every setup-code run as `setup_code_unavailable`, and latches nothing.
    setup_claims: Option<Arc<SetupClaimService>>,
}

/// CPace state between `init_pake` and `verify_pake`. In memory only: a restart forgets every
/// run, as it already forgets every plaintext code.
#[derive(Default)]
struct PakeState {
    /// Runs awaiting their `verify`, by challenge id; removed (and zeroized) on first use.
    pending: HashMap<String, PendingPake>,
    /// Confirmation attempts spent per code, by code hash.
    attempts: HashMap<String, u32>,
    /// Device runs started per `client_id`, paired or not.
    device_resumes: DeviceResumeThrottle,
}

/// One CPace run the Pond has started.
struct PendingPake {
    initiator: Initiator,
    secret: RunSecret,
    transport: RunTransport,
    client_id: String,
    client_type: String,
    expires_at: DateTime<Utc>,
}

/// Which surface started a run. A verify from the other one never burns it, or anyone who
/// overheard a BLE challenge id could spend the phone's run over HTTPS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunTransport {
    Https,
    /// The in-process BLE setup channel: the only transport that receives channel keys.
    SetupChannel,
}

/// The answer to a verify, and the channel keys when a setup-channel run was accepted.
type RunOutcome = (HandshakeResponse, Option<ChannelKeys>);

/// What a completed pairing minted, and whether the device it admitted was already known.
struct CompletedPairing {
    session: String,
    refresh: String,
    expires: DateTime<Utc>,
    /// See [`HandshakeResponse::returning_device`].
    returning: bool,
}

/// The password a run committed to, which decides the limits it is held to.
enum RunSecret {
    /// A pairing code, by hash: its failure budget and its consumption apply.
    Pairing { code_hash: String },
    /// The printed setup code: its guess throttle was charged when the run started.
    Setup,
    /// The device's newest refresh hash, or a random stand-in when it has none; the row a
    /// confirmed run rotates. Its throttle was charged when the run started.
    Device {
        refresh_hash: zeroize::Zeroizing<String>,
    },
}

impl RunSecret {
    fn credential(&self) -> Credential {
        match self {
            Self::Pairing { .. } => Credential::Pairing,
            Self::Setup => Credential::Setup,
            Self::Device { .. } => Credential::Device,
        }
    }
}

impl SqliteHandshakeAdapter {
    /// `spki_pin` is this server's key pin (`sha256/<base64>`), which every CPace channel
    /// identifier carries. Required, not a builder step, so it can't be silently forgotten;
    /// `None` refuses every phone with `channel_binding_mismatch`.
    pub fn new(pool: Pool<Sqlite>, spki_pin: Option<String>) -> Self {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        Self {
            pool,
            hostname,
            spki_pin,
            pake: Mutex::new(PakeState::default()),
            setup_claims: None,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: POND_CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        }
    }

    /// Accept the printed setup code while the pond is unclaimed, and latch the claim on every
    /// completed pairing.
    pub fn with_setup_claims(mut self, claims: Arc<SetupClaimService>) -> Self {
        self.setup_claims = Some(claims);
        self
    }

    pub fn into_dyn(self) -> Arc<dyn Handshake> {
        Arc::new(self)
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn sha256_hex(input: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(input);
        hex_lower(&h.finalize())
    }

    fn random_bytes(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        OsRng.fill_bytes(&mut buf);
        buf
    }

    fn random_token() -> String {
        B64.encode(Self::random_bytes(32))
    }

    fn random_pairing_code() -> String {
        let mut buf = [0u8; 4];
        OsRng.fill_bytes(&mut buf);
        format!("{:06}", u32::from_le_bytes(buf) % 1_000_000)
    }

    fn response(
        &self,
        accepted: bool,
        session_token: Option<String>,
        refresh_token: Option<String>,
        expires_at: Option<String>,
        rejection_reason: Option<String>,
    ) -> HandshakeResponse {
        HandshakeResponse {
            accepted,
            session_token,
            refresh_token,
            expires_at,
            hostname: self.hostname.clone(),
            server_version: self.server_version.clone(),
            capabilities: self.capabilities.clone(),
            rejection_reason,
            // Only an accepted `verify_pake` has a confirmation tag to send.
            server_proof: None,
            // Only `complete_pairing` knows; it sets this on the response it accepts.
            returning_device: false,
        }
    }

    fn reject(&self, reason: &str) -> HandshakeResponse {
        self.response(false, None, None, None, Some(reason.to_string()))
    }

    /// Mint a session pair, revoking the device's active one so re-pairs don't pile up.
    async fn issue_session_pair(
        &self,
        client_id: &str,
        client_type: &str,
        device_id: &str,
    ) -> Result<(String, String, DateTime<Utc>)> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let result = self
            .issue_session_pair_in(&mut tx, client_id, client_type, device_id)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    async fn issue_session_pair_in(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        client_id: &str,
        client_type: &str,
        device_id: &str,
    ) -> Result<(String, String, DateTime<Utc>)> {
        let session_token = Self::random_token();
        let refresh_token = Self::random_token();
        let now = Self::now();
        let expires = now + Duration::hours(SESSION_TTL_HOURS);
        let refresh_expires = now + Duration::days(REFRESH_TTL_DAYS);

        sqlx::query(
            "UPDATE session_tokens SET revoked_at = ?
             WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind(now.to_rfc3339())
        .bind(device_id)
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            "INSERT INTO session_tokens (token_hash, refresh_hash, device_id, client_id,
                client_type, created_at, expires_at, refresh_expires_at, last_seen_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Self::sha256_hex(session_token.as_bytes()))
        .bind(Self::sha256_hex(refresh_token.as_bytes()))
        .bind(device_id)
        .bind(client_id)
        .bind(client_type)
        .bind(now.to_rfc3339())
        .bind(expires.to_rfc3339())
        .bind(refresh_expires.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut **tx)
        .await?;

        Ok((session_token, refresh_token, expires))
    }

    /// The `code_hash` of the active row this plaintext code matches.
    async fn match_pairing_code(&self, code: &str) -> Result<Option<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT code_hash FROM pairing_codes
             WHERE consumed_at IS NULL AND expires_at > ?
             ORDER BY created_at DESC",
        )
        .bind(Self::now().to_rfc3339())
        .fetch_all(&self.pool)
        .await?;

        let candidate = Self::sha256_hex(code.as_bytes());
        for (stored,) in rows {
            if stored.as_bytes().ct_eq(candidate.as_bytes()).into() {
                return Ok(Some(stored));
            }
        }
        Ok(None)
    }

    /// The CPace state; a poisoned lock is recovered, since every write leaves it consistent.
    fn pake_state(&self) -> std::sync::MutexGuard<'_, PakeState> {
        self.pake.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// End these codes now (by hash): expired in the table, plaintext forgotten, budget spent.
    async fn retire_codes(&self, code_hashes: &[String]) -> Result<()> {
        let now = Self::now().to_rfc3339();
        for hash in code_hashes {
            sqlx::query(
                "UPDATE pairing_codes SET expires_at = ? WHERE code_hash = ? AND consumed_at IS NULL",
            )
            .bind(&now)
            .bind(hash)
            .execute(&self.pool)
            .await?;
        }
        {
            let mut cache = ISSUED_CODE_CACHE.write().await;
            for hash in code_hashes {
                cache.remove(hash);
            }
        }
        let mut state = self.pake_state();
        for hash in code_hashes {
            // Spent rather than forgotten: a verify already past its liveness check must not
            // find a fresh budget for a code that has just been retired.
            state.attempts.insert(hash.clone(), PAKE_FAILURE_BUDGET);
        }
        Ok(())
    }

    /// The member an active code was issued for, `Some(None)` for an unattributed one, and
    /// `None` when the code is consumed, expired or retired.
    async fn active_code_profile(&self, code_hash: &str) -> Result<Option<Option<String>>> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT profile_id FROM pairing_codes
             WHERE code_hash = ? AND consumed_at IS NULL AND expires_at > ?",
        )
        .bind(code_hash)
        .bind(Self::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(profile,)| profile))
    }

    /// Any first pairing claims the pond, naming `device_id`, the device the pairing issues its
    /// tokens to. Best effort: a broken setup directory must not take pairing down with it, and
    /// a pond with no setup record has nothing to latch.
    async fn latch_setup_claim(&self, method: &'static str, device_id: &str) {
        let Some(claims) = self.setup_claims.as_ref() else {
            return;
        };
        if let Err(error) = claims.latch_claim(Self::now(), device_id).await {
            tracing::warn!(
                kind = "setup_claim_latch_failed",
                %error,
                method,
                "a device paired, but the setup code's claim could not be recorded; \
                 the printed card may still be accepted"
            );
        }
    }

    /// Write or refresh the device row a pairing registers. Logged, not fatal: the proof was
    /// valid, but the attribution rides on this statement.
    async fn register_device(
        &self,
        device_id: &str,
        device_name: &str,
        client_type: &str,
        profile_id: Option<&str>,
        at: &str,
    ) {
        // On conflict the new attribution wins, so an ordinary code releases an old one:
        // `device_id` is self-reported, and inheriting the owner would allow impersonation.
        let registered = sqlx::query(
            "INSERT INTO devices (id, name, hostname, device_type, ip_address,
                capabilities, last_seen, is_online, created_at, updated_at, profile_id)
             VALUES (?, ?, '', ?, '', '[]', ?, 1, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, device_type = excluded.device_type,
               last_seen = excluded.last_seen,
               is_online = 1, updated_at = excluded.updated_at,
               profile_id = excluded.profile_id",
        )
        .bind(device_id)
        .bind(device_name)
        .bind(client_type)
        .bind(at)
        .bind(at)
        .bind(at)
        .bind(profile_id)
        .execute(&self.pool)
        .await;
        if let Err(e) = &registered {
            tracing::warn!(
                error = %e,
                device = %device_id,
                "device row not written during pairing; the device is unattributed"
            );
        }
    }

    fn device_name_for(client_id: &str, device_name: Option<String>) -> String {
        device_name
            .unwrap_or_else(|| format!("gotg-{}", client_id.chars().take(8).collect::<String>()))
    }

    /// The household's only member, if it has exactly one. A failed read narrows to nobody: a
    /// wrong owner would ride on every turn.
    async fn sole_household_member(&self) -> Option<String> {
        match sqlx::query_scalar::<_, String>("SELECT id FROM profiles")
            .fetch_all(&self.pool)
            .await
        {
            Ok(ids) => sole_member(&ids),
            Err(e) => {
                tracing::warn!(error = %e, "could not list household members; the claiming device is unattributed");
                None
            }
        }
    }

    /// Register the claiming device and mint its tokens. Called only after the claim latched,
    /// and it never touches `pairing_codes`: the setup code is not one.
    async fn complete_setup_claim(
        &self,
        client_id: &str,
        client_type: &str,
        device_name: Option<String>,
    ) -> Result<(String, String, DateTime<Utc>)> {
        let at = Self::now().to_rfc3339();
        let owner = self.sole_household_member().await;
        let device_name = Self::device_name_for(client_id, device_name);
        self.register_device(client_id, &device_name, client_type, owner.as_deref(), &at)
            .await;
        let (session, refresh, expires) = self
            .issue_session_pair(client_id, client_type, client_id)
            .await?;
        tracing::info!(
            client_id = %client_id,
            device = %device_name,
            method = "cpace-setup",
            attributed = owner.is_some(),
            expires_at = %expires.to_rfc3339(),
            "device paired"
        );
        Ok((session, refresh, expires))
    }

    /// The rest of a setup run once its run is burned and live: confirm, latch, then mint.
    async fn verify_setup_run(
        &self,
        initiator: Initiator,
        request: PakeVerifyRequest,
        client_id: String,
        client_type: String,
        transport: RunTransport,
    ) -> Result<RunOutcome> {
        let share = B64.decode(request.share.as_bytes()).unwrap_or_default();
        let confirm = hex_decode(&request.confirm).unwrap_or_default();
        let confirmed = match initiator.finish(&share, &confirm) {
            Ok(confirmed) => confirmed,
            Err(error) => {
                let reason = match error {
                    CpaceError::InvalidShare => "invalid_share",
                    CpaceError::InvalidConfirmation => "invalid_confirmation",
                };
                // The throttle was charged when the run started; nothing else is spent here.
                tracing::warn!(reason, "setup code CPace attempt failed");
                return Ok((self.reject(reason), None));
            }
        };
        let Some(claims) = self.setup_claims.as_ref() else {
            return Ok((self.reject(PakeRefusal::SetupCodeUnavailable.code()), None));
        };
        // Latched before any token exists, so a crash leaves a claimed pond, never a paired phone
        // beside a live card. `complete_setup_claim` issues the tokens to `client_id`.
        if let Err(error) = claims.record_claim(Self::now(), &client_id).await {
            if error.downcast_ref::<ClaimRefusal>().is_some() {
                tracing::warn!(
                    client_id = %client_id,
                    "setup pairing refused: another device claimed this pond first"
                );
            } else {
                tracing::warn!(
                    %error,
                    "setup pairing refused: the claim could not be recorded"
                );
            }
            return Ok((self.reject(PakeRefusal::SetupCodeUnavailable.code()), None));
        }
        let (session, refresh, expires) = self
            .complete_setup_claim(&client_id, &client_type, request.device_name)
            .await?;
        Ok(self.accepted(&confirmed, session, refresh, expires, transport))
    }

    /// The newest active refresh hash for `device_id`, or a random one of the same shape when
    /// there is none, so an unpaired id fails at confirmation exactly as a wrong token does.
    async fn device_run_password(&self, device_id: &str) -> Result<zeroize::Zeroizing<String>> {
        let newest: Option<String> = sqlx::query_scalar(
            "SELECT refresh_hash FROM session_tokens
             WHERE device_id = ? AND revoked_at IS NULL AND refresh_expires_at > ?
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(device_id)
        .bind(Self::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?;
        if let Some(hash) = newest {
            return Ok(zeroize::Zeroizing::new(hash));
        }
        tracing::debug!(
            kind = "device_run_unpaired",
            "no active session for this id; the device run commits to a random password"
        );
        let stand_in = zeroize::Zeroizing::new(Self::random_bytes(32));
        Ok(zeroize::Zeroizing::new(hex_lower(&stand_in)))
    }

    /// The rest of a device run once its run is burned and live: confirm, then rotate. No claim
    /// latch, no code and no `devices` write, so the device keeps its attribution.
    async fn verify_device_run(
        &self,
        initiator: Initiator,
        request: PakeVerifyRequest,
        client_id: String,
        refresh_hash: zeroize::Zeroizing<String>,
        transport: RunTransport,
    ) -> Result<RunOutcome> {
        let share = B64.decode(request.share.as_bytes()).unwrap_or_default();
        let confirm = hex_decode(&request.confirm).unwrap_or_default();
        let confirmed = match initiator.finish(&share, &confirm) {
            Ok(confirmed) => confirmed,
            Err(error) => {
                let reason = match error {
                    CpaceError::InvalidShare => "invalid_share",
                    CpaceError::InvalidConfirmation => "invalid_confirmation",
                };
                // One line for a paired id and an unknown one: which it was is not for the log.
                tracing::warn!(reason, "device run CPace attempt failed");
                return Ok((self.reject(reason), None));
            }
        };
        let Some((session, refresh, expires)) = self.rotate_refresh(&refresh_hash).await? else {
            tracing::warn!(
                "device run refused: the session it proved was rotated or revoked since it started"
            );
            return Ok((self.reject("invalid_or_expired_refresh"), None));
        };
        self.pake_state().device_resumes.record_success(&client_id);
        tracing::info!(
            client_id = %client_id,
            method = "cpace-device",
            expires_at = %expires.to_rfc3339(),
            "device resumed its session"
        );
        Ok(self.accepted(&confirmed, session, refresh, expires, transport))
    }

    /// Revoke this refresh row and mint the next pair for the same device and client, in one
    /// transaction. `None` when the row is revoked, expired or unknown.
    async fn rotate_refresh(
        &self,
        refresh_hash: &str,
    ) -> Result<Option<(String, String, DateTime<Utc>)>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let now = Utc::now().to_rfc3339();
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT client_id, client_type, device_id FROM session_tokens
             WHERE refresh_hash = ? AND revoked_at IS NULL AND refresh_expires_at > ?",
        )
        .bind(refresh_hash)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((client_id, client_type, device_id)) = row else {
            return Ok(None);
        };
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE refresh_hash = ?")
            .bind(&now)
            .bind(refresh_hash)
            .execute(&mut *tx)
            .await?;
        let pair = self
            .issue_session_pair_in(&mut tx, &client_id, &client_type, &device_id)
            .await?;
        tx.commit().await?;
        Ok(Some(pair))
    }

    /// An accepted CPace answer. Channel keys exist only for a setup-channel run, and only now,
    /// after the tokens they will carry were minted.
    fn accepted(
        &self,
        confirmed: &Confirmed,
        session: String,
        refresh: String,
        expires: DateTime<Utc>,
        transport: RunTransport,
    ) -> RunOutcome {
        let response = HandshakeResponse {
            server_proof: Some(hex_lower(&confirmed.server_tag)),
            ..self.response(
                true,
                Some(session),
                Some(refresh),
                Some(expires.to_rfc3339()),
                None,
            )
        };
        let keys = (transport == RunTransport::SetupChannel).then(|| confirmed.channel_keys());
        (response, keys)
    }

    /// Consume the code, register the device and mint tokens, once a proof has verified.
    /// `None` when the code was consumed by another pairing, retired or expired first. Only the code attributes the
    /// device; nothing the client sent is consulted (see `issue_pairing_code_for`).
    async fn complete_pairing(
        &self,
        code_hash: &str,
        client_id: &str,
        client_type: &str,
        device_name: Option<String>,
        method: &'static str,
    ) -> Result<Option<CompletedPairing>> {
        let at = Self::now().to_rfc3339();
        let code_profile: Option<String> =
            sqlx::query_scalar("SELECT profile_id FROM pairing_codes WHERE code_hash = ?")
                .bind(code_hash)
                .fetch_optional(&self.pool)
                .await?
                .flatten();

        let consumed = sqlx::query(
            "UPDATE pairing_codes SET consumed_at = ?
             WHERE code_hash = ? AND consumed_at IS NULL AND expires_at > ?",
        )
        .bind(&at)
        .bind(code_hash)
        .bind(&at)
        .execute(&self.pool)
        .await?;
        ISSUED_CODE_CACHE.write().await.remove(code_hash);
        self.pake_state().attempts.remove(code_hash);
        if consumed.rows_affected() == 0 {
            tracing::warn!(
                client_id = %client_id,
                method,
                "pairing refused: the proof verified, but the code was consumed, retired or expired first"
            );
            return Ok(None);
        }
        let device_id = client_id.to_string();
        self.latch_setup_claim(method, &device_id).await;

        // Read before `register_device` writes the row, or every pairing looks like a return.
        let returning = self.is_returning_device(&device_id, client_type).await;
        let device_name = Self::device_name_for(client_id, device_name);
        self.register_device(
            &device_id,
            &device_name,
            client_type,
            code_profile.as_deref(),
            &at,
        )
        .await;

        let (session, refresh, expires) = self
            .issue_session_pair(client_id, client_type, &device_id)
            .await?;
        tracing::info!(
            kind = "device_paired",
            client_id = %client_id,
            device = %device_name,
            method,
            returning,
            expires_at = %expires.to_rfc3339(),
            "device paired"
        );
        Ok(Some(CompletedPairing {
            session,
            refresh,
            expires,
            returning,
        }))
    }

    /// Whether `device_id` already has a row of this `client_type`, so a pairing re-admits it
    /// rather than admitting a new device. A failed read counts as new: the cost is one extra
    /// notice, where the other answer could hide a device the household has not seen.
    async fn is_returning_device(&self, device_id: &str, client_type: &str) -> bool {
        match sqlx::query_scalar::<_, String>("SELECT device_type FROM devices WHERE id = ?")
            .bind(device_id)
            .fetch_optional(&self.pool)
            .await
        {
            Ok(existing) => existing.as_deref() == Some(client_type),
            Err(error) => {
                tracing::warn!(
                    kind = "pairing_device_lookup_failed",
                    %error,
                    device = %device_id,
                    "could not tell whether a pairing device is new; treating it as new"
                );
                false
            }
        }
    }

    /// CPace phase 1 for either transport; the run remembers which one started it.
    async fn start_pake_run(
        &self,
        request: PakeInitRequest,
        transport: RunTransport,
    ) -> Result<std::result::Result<PakeChallenge, PakeRefusal>> {
        // Before any refusal can return: otherwise runs committed to a code that has gone stay
        // in memory until the next run starts.
        let started = Self::now();
        self.pake_state()
            .pending
            .retain(|_, run| run.expires_at > started);
        // HTTPS has `/handshake/refresh`; a device run there would be a second way in for nothing.
        if request.credential == Credential::Device && transport != RunTransport::SetupChannel {
            tracing::warn!(
                kind = "device_credential_refused",
                "pake_init named the device credential outside the setup channel"
            );
            return Ok(Err(PakeRefusal::InvalidCredential));
        }
        // The run is under this Pond's own pin whatever the phone claims; the claim only lets a
        // phone at the wrong Pond fail here, clearly, instead of at confirmation.
        let Some(pin) = self.spki_pin.as_deref() else {
            tracing::warn!("CPace pairing refused: this Pond has no TLS identity to bind");
            return Ok(Err(PakeRefusal::ChannelBindingMismatch));
        };
        if request.channel_binding != pin {
            tracing::warn!(
                claimed = %request.channel_binding,
                ours = pin,
                "pairing refused: the phone pinned a key that is not this Pond's"
            );
            return Ok(Err(PakeRefusal::ChannelBindingMismatch));
        }
        let nonce = match B64.decode(request.client_nonce.as_bytes()) {
            Ok(nonce) if nonce.len() == pairing_cpace::SID_PART_LEN => nonce,
            _ => return Ok(Err(PakeRefusal::InvalidClientNonce)),
        };

        let (secret, password, ci) = match request.credential {
            Credential::Pairing => {
                // Commit to the newest active code: the one the dashboard and CLI show.
                let newest: Option<(String,)> = sqlx::query_as(
                    "SELECT code_hash FROM pairing_codes
                     WHERE consumed_at IS NULL AND expires_at > ?
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(Self::now().to_rfc3339())
                .fetch_optional(&self.pool)
                .await?;
                let Some((code_hash,)) = newest else {
                    return Ok(Err(PakeRefusal::NoActivePairingCode));
                };
                let Some(code) = ISSUED_CODE_CACHE.read().await.get(&code_hash).cloned() else {
                    // Issued by an earlier process: its plaintext is gone, so nobody can pair with it.
                    return Ok(Err(PakeRefusal::NoActivePairingCode));
                };
                (
                    RunSecret::Pairing { code_hash },
                    zeroize::Zeroizing::new(code),
                    pairing_cpace::channel_identifier(pin, &request.client_id),
                )
            }
            Credential::Setup => {
                let Some(claims) = self.setup_claims.as_ref() else {
                    return Ok(Err(PakeRefusal::SetupCodeUnavailable));
                };
                let code = match claims.admit_setup_attempt(std::time::Instant::now()).await {
                    Ok(code) => code,
                    Err(ClaimRefusal::SetupCodeUnavailable) => {
                        return Ok(Err(PakeRefusal::SetupCodeUnavailable))
                    }
                    Err(ClaimRefusal::SetupCodeCooling { retry_after }) => {
                        tracing::warn!(
                            retry_after_secs = retry_after.as_secs_ceil(),
                            "setup code refused: too many wrong attempts, cooling down"
                        );
                        return Ok(Err(PakeRefusal::SetupCodeCooling { retry_after }));
                    }
                };
                (
                    RunSecret::Setup,
                    zeroize::Zeroizing::new(code.digits().to_string()),
                    pairing_cpace::setup_channel_identifier(pin, &request.client_id),
                )
            }
            Credential::Device => {
                // Charged before the lookup, so a paired id and an unknown one are limited alike.
                let admitted = self
                    .pake_state()
                    .device_resumes
                    .admit(&request.client_id, std::time::Instant::now());
                if let Err(retry_after) = admitted {
                    tracing::warn!(
                        retry_after_secs = retry_after.as_secs_ceil(),
                        "device run refused: too many failed attempts, cooling down"
                    );
                    return Ok(Err(PakeRefusal::DeviceCooling { retry_after }));
                }
                let refresh_hash = self.device_run_password(&request.client_id).await?;
                (
                    RunSecret::Device {
                        refresh_hash: refresh_hash.clone(),
                    },
                    refresh_hash,
                    pairing_cpace::device_channel_identifier(pin, &request.client_id),
                )
            }
        };

        let challenge = Self::random_bytes(pairing_cpace::SID_PART_LEN);
        let initiator = Initiator::start(
            password.as_bytes(),
            &ci,
            pairing_cpace::session_id(&challenge, &nonce),
        );
        let share = initiator.share();
        let id = uuid::Uuid::new_v4().to_string();
        let now = Self::now();
        let expires = now + Duration::seconds(CHALLENGE_TTL_SEC);
        {
            let mut state = self.pake_state();
            state.pending.retain(|_, run| run.expires_at > now);
            if state.pending.len() >= MAX_PENDING_PAKE {
                tracing::warn!(
                    pending = state.pending.len(),
                    "CPace pairing refused: too many runs are waiting for their verify"
                );
                return Ok(Err(PakeRefusal::Busy));
            }
            state.pending.insert(
                id.clone(),
                PendingPake {
                    initiator,
                    secret,
                    transport,
                    client_id: request.client_id,
                    client_type: request.client_type,
                    expires_at: expires,
                },
            );
        }
        Ok(Ok(PakeChallenge {
            challenge_id: id,
            challenge: B64.encode(&challenge),
            expires_at: expires.to_rfc3339(),
            suite: pairing_cpace::SUITE.to_string(),
            share: B64.encode(share),
        }))
    }

    /// CPace phase 2 for either transport. A run is finished only on the transport that started it.
    async fn finish_pake_run(
        &self,
        request: PakeVerifyRequest,
        transport: RunTransport,
    ) -> Result<RunOutcome> {
        // 1. Burn the run before anything is checked: one attempt per challenge.
        let run = {
            let mut state = self.pake_state();
            match state.pending.get(&request.challenge_id) {
                Some(run) if run.transport == transport => {
                    state.pending.remove(&request.challenge_id)
                }
                Some(_) => {
                    tracing::warn!(
                        kind = "pake_transport_mismatch",
                        ?transport,
                        "a verify named a run another transport started; the run is left alone"
                    );
                    None
                }
                None => None,
            }
        };
        let Some(run) = run else {
            return Ok((self.reject("unknown_challenge"), None));
        };
        if run.expires_at < Self::now() {
            return Ok((self.reject("challenge_expired"), None));
        }
        let PendingPake {
            initiator,
            secret,
            client_id,
            client_type,
            ..
        } = run;
        let code_hash = match secret {
            RunSecret::Pairing { code_hash } => code_hash,
            RunSecret::Setup => {
                return self
                    .verify_setup_run(initiator, request, client_id, client_type, transport)
                    .await
            }
            RunSecret::Device { refresh_hash } => {
                return self
                    .verify_device_run(initiator, request, client_id, refresh_hash, transport)
                    .await
            }
        };

        // 2. The code the run committed to must still be live: not consumed by another device,
        //    replaced by a newer code, expired, or retired by its failure budget.
        if self.active_code_profile(&code_hash).await?.is_none() {
            return Ok((self.reject("code_retired"), None));
        }

        // 3. Spend one attempt from the code's budget before the tag is checked, so concurrent
        //    verifies cannot check more tags than the budget allows.
        let attempt = {
            let mut state = self.pake_state();
            let spent = state.attempts.entry(code_hash.clone()).or_insert(0);
            if *spent >= PAKE_FAILURE_BUDGET {
                None
            } else {
                *spent += 1;
                Some(*spent)
            }
        };
        let Some(attempt) = attempt else {
            return Ok((self.reject("code_retired"), None));
        };

        // 4. Check `Yb` and `T_client`. Undecodable input fails the same checks as a bad value.
        let share = B64.decode(request.share.as_bytes()).unwrap_or_default();
        let confirm = hex_decode(&request.confirm).unwrap_or_default();
        let confirmed = match initiator.finish(&share, &confirm) {
            Ok(confirmed) => confirmed,
            Err(error) => {
                let reason = match error {
                    CpaceError::InvalidShare => "invalid_share",
                    CpaceError::InvalidConfirmation => "invalid_confirmation",
                };
                if attempt >= PAKE_FAILURE_BUDGET {
                    self.retire_codes(std::slice::from_ref(&code_hash)).await?;
                    tracing::warn!(
                        attempts = attempt,
                        "pairing code retired: its CPace failure budget is spent; issue a new code"
                    );
                    return Ok((self.reject("code_retired"), None));
                }
                tracing::warn!(
                    reason,
                    attempt,
                    budget = PAKE_FAILURE_BUDGET,
                    "CPace pairing attempt failed"
                );
                return Ok((self.reject(reason), None));
            }
        };

        // 5. Consume the code, register the device, mint tokens, and only then prove ourselves.
        let Some(completed) = self
            .complete_pairing(
                &code_hash,
                &client_id,
                &client_type,
                request.device_name,
                "cpace",
            )
            .await?
        else {
            return Ok((self.reject("code_retired"), None));
        };
        let (mut response, keys) = self.accepted(
            &confirmed,
            completed.session,
            completed.refresh,
            completed.expires,
            transport,
        );
        response.returning_device = completed.returning;
        Ok((response, keys))
    }
}

#[async_trait]
impl Handshake for SqliteHandshakeAdapter {
    /// Legacy single-shot: client presents a pairing code directly.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse> {
        let Some(code) = request.pairing_code.as_deref() else {
            return Ok(self.reject("missing_pairing_code"));
        };
        let Some(matched) = self.match_pairing_code(code).await? else {
            return Ok(self.reject("invalid_or_expired_pairing_code"));
        };

        sqlx::query("UPDATE pairing_codes SET consumed_at = ? WHERE code_hash = ?")
            .bind(Self::now().to_rfc3339())
            .bind(&matched)
            .execute(&self.pool)
            .await?;
        ISSUED_CODE_CACHE.write().await.remove(&matched);

        // Writes no `devices` row, so a legacy pair is always unattributed; a member-bound code
        // consumed here is burned with its attribution discarded (the operator re-issues).
        let device_id = request.client_id.clone();
        self.latch_setup_claim("legacy", &device_id).await;
        let (session, refresh, expires) = self
            .issue_session_pair(&request.client_id, &request.client_type, &device_id)
            .await?;
        Ok(self.response(
            true,
            Some(session),
            Some(refresh),
            Some(expires.to_rfc3339()),
            None,
        ))
    }

    /// Like [`validate_token`](Self::validate_token) but names the caller and device (from the
    /// `devices` row pairing registered, never the client) and leaves `last_seen_at` alone.
    /// `client_id_for_token` is deliberately not overridden; the port derives it from this.
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let token_hash = Self::sha256_hex(token.as_bytes());
        let now = Self::now().to_rfc3339();
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT client_id, device_id FROM session_tokens
             WHERE token_hash = ? AND revoked_at IS NULL AND expires_at > ?",
        )
        .bind(&token_hash)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(client_id, device_id)| TokenCaller {
            client_id,
            device_id,
        }))
    }

    async fn validate_token(&self, token: &str) -> Result<bool> {
        let token_hash = Self::sha256_hex(token.as_bytes());
        let now = Self::now().to_rfc3339();
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT token_hash FROM session_tokens
             WHERE token_hash = ? AND revoked_at IS NULL AND expires_at > ?",
        )
        .bind(&token_hash)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await?;

        if row.is_some() {
            // Best-effort device-tracking touch.
            let _ = sqlx::query("UPDATE session_tokens SET last_seen_at = ? WHERE token_hash = ?")
                .bind(&now)
                .bind(&token_hash)
                .execute(&self.pool)
                .await;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn revoke_device(&self, device_id: &str) -> Result<u64> {
        // Marked, not deleted: to an auditor a deleted row looks like an unknown token.
        let revoked = sqlx::query(
            "UPDATE session_tokens SET revoked_at = ? WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind(Self::now().to_rfc3339())
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        Ok(revoked.rows_affected())
    }

    async fn revoke_token(&self, token: &str) -> Result<()> {
        // Revoke the token's device, so a rotation done while this was queued is revoked too.
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE device_id = (SELECT device_id FROM session_tokens WHERE token_hash = ?) AND revoked_at IS NULL")
            .bind(Self::now().to_rfc3339())
            .bind(Self::sha256_hex(token.as_bytes()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn init_handshake(&self, request: InitRequest) -> Result<ChallengeResponse> {
        let challenge = Self::random_bytes(32);
        let id = uuid::Uuid::new_v4().to_string();
        let now = Self::now();
        let expires = now + Duration::seconds(CHALLENGE_TTL_SEC);

        sqlx::query(
            "INSERT INTO handshake_challenges (id, client_id, client_type, challenge, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&request.client_id)
        .bind(&request.client_type)
        .bind(&challenge)
        .bind(now.to_rfc3339())
        .bind(expires.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(ChallengeResponse {
            challenge_id: id,
            challenge: B64.encode(&challenge),
            expires_at: expires.to_rfc3339(),
        })
    }

    async fn verify_handshake(&self, request: VerifyRequest) -> Result<HandshakeResponse> {
        // 1. Look up + validate the challenge.
        let row: Option<(String, String, Vec<u8>, String, Option<String>)> = sqlx::query_as(
            "SELECT client_id, client_type, challenge, expires_at, consumed_at
             FROM handshake_challenges WHERE id = ?",
        )
        .bind(&request.challenge_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((client_id, client_type, challenge, expires_at, consumed_at)) = row else {
            return Ok(self.reject("unknown_challenge"));
        };
        if consumed_at.is_some() {
            return Ok(self.reject("challenge_used"));
        }
        let exp = DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|e| anyhow!("bad expires_at: {e}"))?;
        if exp < Utc::now() {
            return Ok(self.reject("challenge_expired"));
        }

        // 2. Consume the challenge first: one attempt per challenge, so a wrong guess burns it,
        //    not the pairing code. `consumed_at IS NULL` settles races.
        let consumed = sqlx::query(
            "UPDATE handshake_challenges SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(&request.challenge_id)
        .execute(&self.pool)
        .await?;
        if consumed.rows_affected() == 0 {
            return Ok(self.reject("challenge_used"));
        }

        // 3. Find the active code whose process-local plaintext reproduces the MAC.
        let mac_bytes = match hex_decode(&request.mac) {
            Some(b) => b,
            None => return Ok(self.reject("bad_mac_hex")),
        };
        let codes: Vec<(String,)> = sqlx::query_as(
            "SELECT code_hash FROM pairing_codes
             WHERE consumed_at IS NULL AND expires_at > ?",
        )
        .bind(Utc::now().to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        if codes.is_empty() {
            return Ok(self.reject("no_active_pairing_code"));
        }

        let mut matched: Option<String> = None;
        {
            let cache = ISSUED_CODE_CACHE.read().await;
            for (code_hash,) in &codes {
                if let Some(plaintext) = cache.get(code_hash) {
                    if verify_mac(
                        plaintext.as_bytes(),
                        &challenge,
                        client_id.as_bytes(),
                        &mac_bytes,
                    ) {
                        matched = Some(code_hash.clone());
                        break;
                    }
                }
            }
        }
        let Some(code_hash) = matched else {
            return Ok(self.reject("invalid_mac"));
        };

        // 4. Consume the code, register the device, mint tokens.
        let Some(completed) = self
            .complete_pairing(
                &code_hash,
                &client_id,
                &client_type,
                request.device_name,
                "loopback_hmac",
            )
            .await?
        else {
            return Ok(self.reject("code_retired"));
        };
        Ok(HandshakeResponse {
            returning_device: completed.returning,
            ..self.response(
                true,
                Some(completed.session),
                Some(completed.refresh),
                Some(completed.expires.to_rfc3339()),
                None,
            )
        })
    }

    async fn init_pake(
        &self,
        request: PakeInitRequest,
    ) -> Result<std::result::Result<PakeChallenge, PakeRefusal>> {
        self.start_pake_run(request, RunTransport::Https).await
    }

    async fn verify_pake(&self, request: PakeVerifyRequest) -> Result<HandshakeResponse> {
        let (response, _no_keys) = self.finish_pake_run(request, RunTransport::Https).await?;
        Ok(response)
    }

    async fn init_channel_pake(
        &self,
        request: PakeInitRequest,
    ) -> Result<std::result::Result<PakeChallenge, PakeRefusal>> {
        self.start_pake_run(request, RunTransport::SetupChannel)
            .await
    }

    async fn verify_channel_pake(&self, request: PakeVerifyRequest) -> Result<ChannelVerify> {
        let (response, keys) = self
            .finish_pake_run(request, RunTransport::SetupChannel)
            .await?;
        Ok(ChannelVerify { response, keys })
    }

    async fn pake_credential(&self, challenge_id: &str) -> Option<Credential> {
        self.pake_state()
            .pending
            .get(challenge_id)
            .map(|run| run.secret.credential())
    }

    async fn refresh(&self, request: RefreshRequest) -> Result<HandshakeResponse> {
        let refresh_hash = Self::sha256_hex(request.refresh_token.as_bytes());
        let Some((session, refresh, expires)) = self.rotate_refresh(&refresh_hash).await? else {
            return Ok(self.reject("invalid_or_expired_refresh"));
        };
        Ok(self.response(
            true,
            Some(session),
            Some(refresh),
            Some(expires.to_rfc3339()),
            None,
        ))
    }

    async fn issue_pairing_code_for(&self, profile_id: Option<&str>) -> Result<PairingCode> {
        // A blank id is refused: `ON DELETE SET NULL` could never clear it.
        let profile_id = profile_id.map(checked_profile_id).transpose()?;
        let code = Self::random_pairing_code();
        let code_hash = Self::sha256_hex(code.as_bytes());
        let now = Utc::now();
        let expires = now + Duration::minutes(PAIRING_CODE_TTL_MIN);

        // The code on screen is the only one that works: a new code retires every older one,
        // and with it any CPace run committed to one of them.
        let older: Vec<String> = sqlx::query_scalar(
            "SELECT code_hash FROM pairing_codes WHERE consumed_at IS NULL AND expires_at > ?",
        )
        .bind(now.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        if !older.is_empty() {
            self.retire_codes(&older).await?;
            tracing::info!(
                retired = older.len(),
                "a new pairing code retired the older active ones"
            );
        }

        // The `profile_id` foreign key is the existence check for the member.
        sqlx::query(
            "INSERT INTO pairing_codes (code_hash, created_at, expires_at, profile_id) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&code_hash)
        .bind(now.to_rfc3339())
        .bind(expires.to_rfc3339())
        .bind(profile_id)
        .execute(&self.pool)
        .await?;
        ISSUED_CODE_CACHE
            .write()
            .await
            .insert(code_hash, code.clone());
        tracing::info!(
            expires_at = %expires.to_rfc3339(),
            attributed = profile_id.is_some(),
            "issued pairing code (valid {PAIRING_CODE_TTL_MIN}m)"
        );

        Ok(PairingCode {
            code,
            expires_at: expires.to_rfc3339(),
            profile_id: profile_id.map(str::to_string),
        })
    }

    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT code_hash, expires_at, profile_id FROM pairing_codes
             WHERE consumed_at IS NULL AND expires_at > ?
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?;
        let Some((hash, expires_at, profile_id)) = row else {
            return Ok(None);
        };
        let cache = ISSUED_CODE_CACHE.read().await;
        Ok(cache.get(&hash).map(|code| PairingCode {
            code: code.clone(),
            expires_at,
            profile_id: profile_id.clone(),
        }))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    // ASCII first: slicing two bytes at a time would split a multi-byte character and panic.
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        out.push(u8::from_str_radix(&s[i..i + 2], 16).ok()?);
    }
    Some(out)
}

/// The loopback dashboard's MAC, keyed by the pairing code: `HMAC-SHA256(code, challenge ||
/// client_id)`. Loopback only: it binds no channel, and a captured MAC gives up the six digits
/// offline, which is why phones pair with CPace instead.
fn pair_mac(key: &[u8], challenge: &[u8], client_id: &[u8]) -> Option<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(challenge);
    mac.update(client_id);
    Some(mac.finalize().into_bytes().to_vec())
}

fn verify_mac(key: &[u8], challenge: &[u8], client_id: &[u8], expected: &[u8]) -> bool {
    let Some(computed) = pair_mac(key, challenge, client_id) else {
        return false;
    };
    computed.ct_eq(expected).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_device_attribution::SqliteDeviceAttribution;
    use pond_core::user_data::ports::device_attribution::DeviceAttribution;
    use tempfile::tempdir;

    /// This Pond's own pin, as `tls_identity::pin()` spells one.
    const OURS: &str = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";
    /// Somebody else's, for an interceptor to serve.
    const THEIRS: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    /// A code a newer one retired can never complete a pairing, even for a verify that checked it
    /// while it was live, and its budget reads spent.
    #[tokio::test]
    async fn a_retired_code_cannot_complete_a_pairing() {
        let hs = fresh().await;
        let old = hs.issue_pairing_code().await.unwrap();
        let _newer = hs.issue_pairing_code().await.unwrap();
        let old_hash = SqliteHandshakeAdapter::sha256_hex(old.code.as_bytes());
        assert_eq!(
            hs.pake_state().attempts.get(&old_hash),
            Some(&PAKE_FAILURE_BUDGET)
        );
        assert!(hs
            .complete_pairing(&old_hash, "phone-1", "gotg", None, "test")
            .await
            .unwrap()
            .is_none());
    }

    /// A fresh Pond with a TLS identity, `OURS`.
    async fn fresh() -> SqliteHandshakeAdapter {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let adapter = SqliteHandshakeAdapter::new(db.system.clone(), Some(OURS.to_string()));
        // Keep the tempdir alive, or the sqlite file vanishes under the pool.
        std::mem::forget(tmp);
        adapter
    }

    /// As `fresh`, plus the pool and two household members.
    async fn fresh_with_household() -> (SqliteHandshakeAdapter, Pool<Sqlite>) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for (id, name) in [("liz", "Liz"), ("jerry", "Jerry")] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&db.system)
                .await
                .unwrap();
        }
        let adapter = SqliteHandshakeAdapter::new(db.system.clone(), Some(OURS.to_string()));
        std::mem::forget(tmp);
        (adapter, db.system)
    }

    /// Pair the way a phone does, with CPace under the Pond's pin.
    async fn pair(hs: &SqliteHandshakeAdapter, code: &str, client_id: &str) -> HandshakeResponse {
        let (resp, _) = pake_pair(hs, code, client_id, OURS).await;
        resp
    }

    /// A CPace init that the Pond accepts.
    async fn pake_init(
        hs: &SqliteHandshakeAdapter,
        client_id: &str,
        nonce: &[u8],
    ) -> PakeChallenge {
        hs.init_pake(pake_init_request(client_id, OURS, nonce))
            .await
            .unwrap()
            .expect("the Pond starts a run")
    }

    fn pake_init_request(client_id: &str, pin: &str, nonce: &[u8]) -> PakeInitRequest {
        PakeInitRequest {
            client_id: client_id.into(),
            client_type: "gotg".into(),
            client_version: "1.0".into(),
            channel_binding: pin.into(),
            client_nonce: B64.encode(nonce),
            credential: Credential::Pairing,
        }
    }

    /// The phone's answer to `challenge`, computed with `code` under `pin`.
    fn pake_answer(
        challenge: &PakeChallenge,
        code: &str,
        client_id: &str,
        pin: &str,
        nonce: &[u8],
    ) -> (pairing_cpace::Responder, PakeVerifyRequest) {
        let phone = pairing_cpace::Responder::respond(
            code.as_bytes(),
            &pairing_cpace::channel_identifier(pin, client_id),
            &pairing_cpace::session_id(&B64.decode(&challenge.challenge).unwrap(), nonce),
            &B64.decode(&challenge.share).unwrap(),
        )
        .unwrap();
        let request = PakeVerifyRequest {
            challenge_id: challenge.challenge_id.clone(),
            share: B64.encode(phone.share()),
            confirm: hex_lower(&phone.confirmation()),
            device_name: Some(format!("{client_id} phone")),
        };
        (phone, request)
    }

    /// A whole CPace pairing: the phone's code is `code`, and it pinned `pin` but always
    /// claims `OURS`, as an interceptor relaying it would.
    async fn pake_pair(
        hs: &SqliteHandshakeAdapter,
        code: &str,
        client_id: &str,
        pin: &str,
    ) -> (HandshakeResponse, pairing_cpace::Responder) {
        let nonce = [5u8; 32];
        let challenge = pake_init(hs, client_id, &nonce).await;
        let (phone, request) = pake_answer(&challenge, code, client_id, pin, &nonce);
        (hs.verify_pake(request).await.unwrap(), phone)
    }

    fn client_mac(code: &str, challenge_b64: &str, client_id: &str) -> String {
        let challenge = B64.decode(challenge_b64.as_bytes()).unwrap();
        let mut mac = HmacSha256::new_from_slice(code.as_bytes()).unwrap();
        mac.update(&challenge);
        mac.update(client_id.as_bytes());
        hex_lower(&mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn full_two_phase_flow_succeeds_and_token_validates() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: "device-A".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-A"),
                device_name: Some("Test Phone".into()),
            })
            .await
            .unwrap();

        assert!(resp.accepted);
        assert!(resp.capabilities.iter().any(|c| c == "routines"));
        // A 'gotg' client still persists device_type='gotg' end to end.
        let (dtype,): (String,) = sqlx::query_as("SELECT device_type FROM devices WHERE id = ?")
            .bind("device-A")
            .fetch_one(&hs.pool)
            .await
            .unwrap();
        assert_eq!(dtype, "gotg");
        let (ctype,): (String,) = sqlx::query_as(
            "SELECT client_type FROM session_tokens WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind("device-A")
        .fetch_one(&hs.pool)
        .await
        .unwrap();
        assert_eq!(ctype, "gotg");

        let token = resp.session_token.unwrap();
        assert!(hs.validate_token(&token).await.unwrap());
        hs.revoke_token(&token).await.unwrap();
        assert!(!hs.validate_token(&token).await.unwrap());
    }

    #[tokio::test]
    async fn verify_persists_real_client_type_not_hardcoded_gotg() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: "device-DT".into(),
                client_type: "desktop".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-DT"),
                device_name: Some("Pond Desktop".into()),
            })
            .await
            .unwrap();
        assert!(resp.accepted);

        let (dtype,): (String,) = sqlx::query_as("SELECT device_type FROM devices WHERE id = ?")
            .bind("device-DT")
            .fetch_one(&hs.pool)
            .await
            .unwrap();
        assert_eq!(dtype, "desktop");

        let (ctype,): (String,) = sqlx::query_as(
            "SELECT client_type FROM session_tokens WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind("device-DT")
        .fetch_one(&hs.pool)
        .await
        .unwrap();
        assert_eq!(ctype, "desktop");
    }

    #[tokio::test]
    async fn wrong_code_is_rejected() {
        let hs = fresh().await;
        let _ = hs.issue_pairing_code().await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: "device-B".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac("000000", &init.challenge, "device-B"),
                device_name: None,
            })
            .await
            .unwrap();
        assert!(!resp.accepted);
        assert_eq!(resp.rejection_reason.as_deref(), Some("invalid_mac"));
    }

    #[tokio::test]
    async fn re_pair_same_device_revokes_prior_session() {
        let hs = fresh().await;

        let pc1 = hs.issue_pairing_code().await.unwrap();
        let init1 = hs
            .init_handshake(InitRequest {
                client_id: "device-D".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let token1 = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init1.challenge_id,
                mac: client_mac(&pc1.code, &init1.challenge, "device-D"),
                device_name: Some("Phone".into()),
            })
            .await
            .unwrap()
            .session_token
            .unwrap();
        assert!(hs.validate_token(&token1).await.unwrap());

        let pc2 = hs.issue_pairing_code().await.unwrap();
        let init2 = hs
            .init_handshake(InitRequest {
                client_id: "device-D".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let token2 = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init2.challenge_id,
                mac: client_mac(&pc2.code, &init2.challenge, "device-D"),
                device_name: Some("Phone".into()),
            })
            .await
            .unwrap()
            .session_token
            .unwrap();

        assert!(
            !hs.validate_token(&token1).await.unwrap(),
            "old session revoked"
        );
        assert!(
            hs.validate_token(&token2).await.unwrap(),
            "new session live"
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_cannot_escape_authenticated_device_revocation() {
        let hs = fresh().await;
        for _ in 0..20 {
            let (session, refresh, _) = hs
                .issue_session_pair("race-device", "gotg", "race-device")
                .await
                .unwrap();
            let (rotated, revoked) = tokio::join!(
                hs.refresh(RefreshRequest {
                    refresh_token: refresh
                }),
                hs.revoke_token(&session),
            );
            revoked.unwrap();
            let rotated = rotated.unwrap();
            if let Some(token) = rotated.session_token {
                assert!(
                    !hs.validate_token(&token).await.unwrap(),
                    "rotation escaped revocation"
                );
            }
            let (active,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM session_tokens WHERE device_id = ? AND revoked_at IS NULL",
            )
            .bind("race-device")
            .fetch_one(&hs.pool)
            .await
            .unwrap();
            assert_eq!(active, 0);
        }
    }

    #[tokio::test]
    async fn refresh_rotates_tokens() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: "device-C".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-C"),
                device_name: None,
            })
            .await
            .unwrap();
        let old_session = resp.session_token.clone().unwrap();
        let refresh = resp.refresh_token.unwrap();

        let resp2 = hs
            .refresh(RefreshRequest {
                refresh_token: refresh,
            })
            .await
            .unwrap();
        assert!(resp2.accepted);
        let new_session = resp2.session_token.unwrap();
        assert_ne!(old_session, new_session);
        assert!(
            !hs.validate_token(&old_session).await.unwrap(),
            "old revoked"
        );
        assert!(hs.validate_token(&new_session).await.unwrap(), "new live");
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: "device-E".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let token = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-E"),
                device_name: None,
            })
            .await
            .unwrap()
            .session_token
            .unwrap();
        assert!(
            hs.validate_token(&token).await.unwrap(),
            "fresh token valid"
        );

        let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
        sqlx::query("UPDATE session_tokens SET expires_at = ? WHERE token_hash = ?")
            .bind(&past)
            .bind(SqliteHandshakeAdapter::sha256_hex(token.as_bytes()))
            .execute(&hs.pool)
            .await
            .unwrap();
        assert!(
            !hs.validate_token(&token).await.unwrap(),
            "expired token must be rejected"
        );
    }

    async fn init_for(hs: &SqliteHandshakeAdapter, client_id: &str) -> ChallengeResponse {
        hs.init_handshake(InitRequest {
            client_id: client_id.into(),
            client_type: "gotg".into(),
            client_version: "1.0".into(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn bad_mac_attempts_do_not_lock_out_pairing_code() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();

        for _ in 0..10 {
            let init = init_for(&hs, "attacker").await;
            let resp = hs
                .verify_handshake(VerifyRequest {
                    challenge_id: init.challenge_id,
                    mac: client_mac("999999", &init.challenge, "attacker"),
                    device_name: None,
                })
                .await
                .unwrap();
            assert_eq!(resp.rejection_reason.as_deref(), Some("invalid_mac"));
        }

        // The operator's code is still valid — the legitimate device pairs.
        let init = init_for(&hs, "device-Z").await;
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-Z"),
                device_name: None,
            })
            .await
            .unwrap();
        assert!(
            resp.accepted,
            "bad attempts must not lock out the pairing code"
        );
    }

    #[tokio::test]
    async fn challenge_is_single_use_even_on_failure() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = init_for(&hs, "device-R").await;

        let bad = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id.clone(),
                mac: client_mac("000000", &init.challenge, "device-R"),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(bad.rejection_reason.as_deref(), Some("invalid_mac"));

        // Same challenge, now with the CORRECT MAC — must still be refused.
        let reuse = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-R"),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(reuse.rejection_reason.as_deref(), Some("challenge_used"));
    }

    #[tokio::test]
    async fn expired_challenge_is_rejected() {
        let hs = fresh().await;
        let _pc = hs.issue_pairing_code().await.unwrap();
        let init = init_for(&hs, "device-X").await;

        let past = (Utc::now() - Duration::minutes(5)).to_rfc3339();
        sqlx::query("UPDATE handshake_challenges SET expires_at = ? WHERE id = ?")
            .bind(&past)
            .bind(&init.challenge_id)
            .execute(&hs.pool)
            .await
            .unwrap();

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: "00".into(),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.rejection_reason.as_deref(), Some("challenge_expired"));
    }

    #[tokio::test]
    async fn refresh_token_cannot_be_reused_after_rotation() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = init_for(&hs, "device-Q").await;
        let paired = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-Q"),
                device_name: None,
            })
            .await
            .unwrap();
        let old_refresh = paired.refresh_token.unwrap();

        let rotated = hs
            .refresh(RefreshRequest {
                refresh_token: old_refresh.clone(),
            })
            .await
            .unwrap();
        assert!(rotated.accepted, "first refresh should rotate");

        let replay = hs
            .refresh(RefreshRequest {
                refresh_token: old_refresh,
            })
            .await
            .unwrap();
        assert!(
            !replay.accepted,
            "rotated refresh token must not be replayable"
        );
        assert_eq!(
            replay.rejection_reason.as_deref(),
            Some("invalid_or_expired_refresh")
        );
    }

    // ── Device-to-profile rung ────────────────────────────────────────────
    // Through the real two-phase flow: never set `devices.profile_id` by hand in these tests.

    #[tokio::test]
    async fn pairing_with_a_members_code_makes_the_device_theirs() {
        let (hs, pool) = fresh_with_household().await;
        let pc = hs.issue_pairing_code_for(Some("liz")).await.unwrap();
        assert_eq!(pc.profile_id.as_deref(), Some("liz"));

        assert!(pair(&hs, &pc.code, "device-liz").await.accepted);

        let attribution = SqliteDeviceAttribution::new(pool);
        assert_eq!(
            attribution.device_profile("device-liz").await.unwrap(),
            Some("liz".to_string()),
            "the paired device must belong to the member the code was issued for"
        );
        assert_eq!(
            attribution.devices_for_profile("liz").await.unwrap(),
            vec!["device-liz".to_string()]
        );
        assert!(
            attribution
                .devices_for_profile("jerry")
                .await
                .unwrap()
                .is_empty(),
            "Liz's phone is not Jerry's"
        );
    }

    #[tokio::test]
    async fn pairing_with_an_ordinary_code_leaves_the_device_unattributed() {
        let (hs, pool) = fresh_with_household().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        assert_eq!(pc.profile_id, None);

        assert!(pair(&hs, &pc.code, "tablet").await.accepted);

        let attribution = SqliteDeviceAttribution::new(pool);
        assert_eq!(attribution.device_profile("tablet").await.unwrap(), None);
        assert!(attribution
            .devices_for_profile("liz")
            .await
            .unwrap()
            .is_empty());
    }

    /// `PairedDevice` outranks every other identity rung, so the device must not name its member.
    #[tokio::test]
    async fn the_pairing_client_cannot_name_its_own_member() {
        let (hs, pool) = fresh_with_household().await;
        let pc = hs.issue_pairing_code_for(Some("liz")).await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: "device-liz".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();

        // A real client body plus an impersonation field `VerifyRequest` must keep ignoring.
        let body = serde_json::json!({
            "challenge_id": init.challenge_id,
            "mac": client_mac(&pc.code, &init.challenge, "device-liz"),
            "device_name": "Liz Phone",
            "profile_id": "jerry",
        });
        let request: VerifyRequest = serde_json::from_value(body).unwrap();
        assert!(hs.verify_handshake(request).await.unwrap().accepted);

        let attribution = SqliteDeviceAttribution::new(pool);
        assert_eq!(
            attribution.device_profile("device-liz").await.unwrap(),
            Some("liz".to_string()),
            "the code decides the member; the pairing client does not"
        );
        assert!(
            attribution
                .devices_for_profile("jerry")
                .await
                .unwrap()
                .is_empty(),
            "a client that asked to be Jerry must not become Jerry"
        );
    }

    #[tokio::test]
    async fn re_pairing_with_an_ordinary_code_releases_the_previous_member() {
        let (hs, pool) = fresh_with_household().await;
        let liz_code = hs.issue_pairing_code_for(Some("liz")).await.unwrap();
        assert!(pair(&hs, &liz_code.code, "device-x").await.accepted);
        let attribution = SqliteDeviceAttribution::new(pool.clone());
        assert_eq!(
            attribution.device_profile("device-x").await.unwrap(),
            Some("liz".to_string())
        );

        let plain = hs.issue_pairing_code().await.unwrap();
        assert!(pair(&hs, &plain.code, "device-x").await.accepted);
        assert_eq!(
            attribution.device_profile("device-x").await.unwrap(),
            None,
            "an unattributed re-pair must not inherit the previous member"
        );
    }

    #[tokio::test]
    async fn deleting_a_member_releases_their_outstanding_pairing_code() {
        let (hs, pool) = fresh_with_household().await;
        let pc = hs.issue_pairing_code_for(Some("liz")).await.unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .expect("deleting a member with an outstanding code must succeed");

        assert_eq!(
            hs.current_pairing_code().await.unwrap().unwrap().profile_id,
            None,
            "the code survives the member, unattributed"
        );
        assert!(pair(&hs, &pc.code, "device-y").await.accepted);
        let attribution = SqliteDeviceAttribution::new(pool);
        assert_eq!(attribution.device_profile("device-y").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_code_cannot_be_issued_for_a_member_who_does_not_exist() {
        let (hs, _pool) = fresh_with_household().await;
        assert!(
            hs.issue_pairing_code_for(Some("ghost")).await.is_err(),
            "the foreign key must refuse an unknown member"
        );
        assert!(
            hs.issue_pairing_code_for(Some("   ")).await.is_err(),
            "a blank id is neither NULL nor a member"
        );
    }

    // ---- CPace: how phones pair ----------------------------------------------

    async fn challenge_for(hs: &SqliteHandshakeAdapter, client_id: &str) -> ChallengeResponse {
        hs.init_handshake(InitRequest {
            client_id: client_id.into(),
            client_type: "gotg".into(),
            client_version: "1.0".into(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_phone_pairs_with_cpace_and_can_check_the_ponds_proof() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let (resp, phone) = pake_pair(&hs, &pc.code, "device-cpace", OURS).await;

        assert!(resp.accepted, "{:?}", resp.rejection_reason);
        let proof = resp
            .server_proof
            .as_deref()
            .expect("an accepted pair carries T_server");
        assert!(phone.verify_server(&hex_decode(proof).unwrap()));
        assert!(hs
            .validate_token(&resp.session_token.unwrap())
            .await
            .unwrap());
        let (dtype,): (String,) = sqlx::query_as("SELECT device_type FROM devices WHERE id = ?")
            .bind("device-cpace")
            .fetch_one(&hs.pool)
            .await
            .unwrap();
        assert_eq!(dtype, "gotg");
        assert!(
            hs.current_pairing_code().await.unwrap().is_none(),
            "the code is single use"
        );
    }

    #[tokio::test]
    async fn a_wrong_code_fails_confirmation_and_the_right_one_still_pairs() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let wrong = if pc.code == "000000" {
            "000001"
        } else {
            "000000"
        };

        let (resp, _) = pake_pair(&hs, wrong, "device-typo", OURS).await;
        assert!(!resp.accepted);
        assert_eq!(
            resp.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        assert_eq!(
            resp.server_proof, None,
            "a failed run must not reveal T_server"
        );

        let (resp, _) = pake_pair(&hs, &pc.code, "device-typo", OURS).await;
        assert!(resp.accepted, "{:?}", resp.rejection_reason);
    }

    /// The phone names the key it pinned; an interceptor's key is refused before any run.
    #[tokio::test]
    async fn a_phone_that_names_an_interceptors_key_is_refused_at_init() {
        let hs = fresh().await;
        hs.issue_pairing_code().await.unwrap();
        let refused = hs
            .init_pake(pake_init_request("device-mitm", THEIRS, &[1u8; 32]))
            .await
            .unwrap();
        assert_eq!(refused.unwrap_err(), PakeRefusal::ChannelBindingMismatch);
    }

    /// The claim is only a diagnostic: an interceptor relaying a phone that pinned its key can
    /// claim ours, and the run still fails, because the phone's generator used the other pin.
    #[tokio::test]
    async fn a_phone_that_pinned_an_interceptors_key_cannot_pair_even_through_a_relay() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let (resp, _) = pake_pair(&hs, &pc.code, "device-relay", THEIRS).await;
        assert!(!resp.accepted);
        assert_eq!(
            resp.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
    }

    #[tokio::test]
    async fn a_pond_with_no_tls_identity_refuses_every_phone() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let hs = SqliteHandshakeAdapter::new(db.system.clone(), None);
        hs.issue_pairing_code().await.unwrap();
        let refused = hs
            .init_pake(pake_init_request("device-noid", OURS, &[1u8; 32]))
            .await
            .unwrap();
        assert_eq!(refused.unwrap_err(), PakeRefusal::ChannelBindingMismatch);
    }

    #[tokio::test]
    async fn init_refuses_without_a_code_or_with_a_malformed_nonce() {
        let hs = fresh().await;
        let refused = hs
            .init_pake(pake_init_request("device-early", OURS, &[1u8; 32]))
            .await
            .unwrap();
        assert_eq!(refused.unwrap_err(), PakeRefusal::NoActivePairingCode);

        hs.issue_pairing_code().await.unwrap();
        let refused = hs
            .init_pake(pake_init_request("device-nonce", OURS, &[1u8; 16]))
            .await
            .unwrap();
        assert_eq!(refused.unwrap_err(), PakeRefusal::InvalidClientNonce);
    }

    /// Ten failed confirmations retire the code, and runs already started on it die with it.
    #[tokio::test]
    async fn the_failure_budget_retires_the_code_after_ten_attempts() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let wrong = if pc.code == "000000" {
            "000001"
        } else {
            "000000"
        };

        // Started before the budget is spent, with the right code.
        let nonce = [3u8; 32];
        let honest = pake_init(&hs, "device-late", &nonce).await;

        for attempt in 1..=PAKE_FAILURE_BUDGET {
            let (resp, _) = pake_pair(&hs, wrong, "attacker", OURS).await;
            let expected = if attempt < PAKE_FAILURE_BUDGET {
                "invalid_confirmation"
            } else {
                "code_retired"
            };
            assert_eq!(
                resp.rejection_reason.as_deref(),
                Some(expected),
                "attempt {attempt}"
            );
        }

        let (_, request) = pake_answer(&honest, &pc.code, "device-late", OURS, &nonce);
        let resp = hs.verify_pake(request).await.unwrap();
        assert_eq!(resp.rejection_reason.as_deref(), Some("code_retired"));
        assert!(hs.current_pairing_code().await.unwrap().is_none());
        let refused = hs
            .init_pake(pake_init_request("device-late", OURS, &nonce))
            .await
            .unwrap();
        assert_eq!(refused.unwrap_err(), PakeRefusal::NoActivePairingCode);

        // A new code starts a new budget.
        let fresh_code = hs.issue_pairing_code().await.unwrap();
        assert!(pair(&hs, &fresh_code.code, "device-late").await.accepted);
    }

    /// A malformed share is a failed attempt too; it spends the budget like a wrong tag.
    #[tokio::test]
    async fn malformed_shares_are_refused_and_counted() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        for share in [B64.encode([0u8; 32]), "not base64".to_string()] {
            let challenge = pake_init(&hs, "device-bad", &[1u8; 32]).await;
            let resp = hs
                .verify_pake(PakeVerifyRequest {
                    challenge_id: challenge.challenge_id,
                    share,
                    confirm: "00".repeat(64),
                    device_name: None,
                })
                .await
                .unwrap();
            assert_eq!(resp.rejection_reason.as_deref(), Some("invalid_share"));
        }
        // A non-ASCII tag is refused, not a panic in the handler.
        let challenge = pake_init(&hs, "device-bad", &[1u8; 32]).await;
        let resp = hs
            .verify_pake(PakeVerifyRequest {
                challenge_id: challenge.challenge_id,
                share: challenge.share,
                confirm: "\u{e9}".repeat(64),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(
            resp.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        let code_hash = SqliteHandshakeAdapter::sha256_hex(pc.code.as_bytes());
        assert_eq!(hs.pake_state().attempts.get(&code_hash), Some(&3));
    }

    #[tokio::test]
    async fn issuing_a_code_retires_every_older_one() {
        let hs = fresh().await;
        let first = hs.issue_pairing_code().await.unwrap();
        let nonce = [4u8; 32];
        let on_first = pake_init(&hs, "device-old", &nonce).await;
        let hmac_on_first = challenge_for(&hs, "dashboard-old").await;

        let second = hs.issue_pairing_code().await.unwrap();
        assert_eq!(
            hs.current_pairing_code().await.unwrap().map(|c| c.code),
            Some(second.code.clone())
        );

        // A run committed to the old code dies with it...
        let (_, request) = pake_answer(&on_first, &first.code, "device-old", OURS, &nonce);
        let resp = hs.verify_pake(request).await.unwrap();
        assert_eq!(resp.rejection_reason.as_deref(), Some("code_retired"));
        // ...and the dashboard's MAC under it no longer matches any code.
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: hmac_on_first.challenge_id,
                mac: client_mac(&first.code, &hmac_on_first.challenge, "dashboard-old"),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.rejection_reason.as_deref(), Some("invalid_mac"));

        assert!(pair(&hs, &second.code, "device-new").await.accepted);
    }

    #[tokio::test]
    async fn a_cpace_challenge_is_single_use_and_not_an_hmac_one() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let nonce = [6u8; 32];
        let challenge = pake_init(&hs, "device-once", &nonce).await;
        let (_, wrong) = pake_answer(&challenge, "999999", "device-once", OURS, &nonce);
        let (_, right) = pake_answer(&challenge, &pc.code, "device-once", OURS, &nonce);
        assert!(!hs.verify_pake(wrong).await.unwrap().accepted);
        let reuse = hs.verify_pake(right).await.unwrap();
        assert_eq!(reuse.rejection_reason.as_deref(), Some("unknown_challenge"));

        // The two exchanges keep separate challenges: neither completes the other's.
        let hmac = challenge_for(&hs, "dashboard").await;
        let crossed = hs
            .verify_pake(PakeVerifyRequest {
                challenge_id: hmac.challenge_id,
                share: B64.encode([1u8; 32]),
                confirm: String::new(),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(
            crossed.rejection_reason.as_deref(),
            Some("unknown_challenge")
        );
    }

    #[tokio::test]
    async fn an_expired_cpace_challenge_is_refused() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let nonce = [8u8; 32];
        let challenge = pake_init(&hs, "device-slow", &nonce).await;
        hs.pake_state()
            .pending
            .get_mut(&challenge.challenge_id)
            .unwrap()
            .expires_at = Utc::now() - Duration::seconds(1);
        let (_, request) = pake_answer(&challenge, &pc.code, "device-slow", OURS, &nonce);
        let resp = hs.verify_pake(request).await.unwrap();
        assert_eq!(resp.rejection_reason.as_deref(), Some("challenge_expired"));
    }

    #[tokio::test]
    async fn waiting_runs_are_capped() {
        let hs = fresh().await;
        hs.issue_pairing_code().await.unwrap();
        for _ in 0..MAX_PENDING_PAKE {
            pake_init(&hs, "flood", &[2u8; 32]).await;
        }
        let refused = hs
            .init_pake(pake_init_request("one-more", OURS, &[2u8; 32]))
            .await
            .unwrap();
        assert_eq!(refused.unwrap_err(), PakeRefusal::Busy);
    }

    /// `session_tokens.device_id` has no foreign key, so nothing cascades from `devices`.
    #[tokio::test]
    async fn revoking_a_device_ends_every_session_it_holds() {
        let hs = fresh().await;
        let first = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        let other = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "tablet").await;
        let phone = first.session_token.unwrap();
        let tablet = other.session_token.unwrap();
        assert!(hs.validate_token(&phone).await.unwrap());
        assert!(hs.validate_token(&tablet).await.unwrap());

        // The device id is the client id this adapter derives it from.
        assert_eq!(hs.revoke_device("phone").await.unwrap(), 1);
        assert!(!hs.validate_token(&phone).await.unwrap());
        assert!(
            hs.validate_token(&tablet).await.unwrap(),
            "revoking one device must not disconnect the household"
        );
        // The refresh token dies too, or the phone would just mint a new session.
        let refreshed = hs
            .refresh(RefreshRequest {
                refresh_token: first.refresh_token.unwrap(),
            })
            .await
            .unwrap();
        assert!(
            !refreshed.accepted,
            "a revoked device refreshed back into a session"
        );
    }

    #[tokio::test]
    async fn revoking_a_device_with_no_sessions_is_not_an_error() {
        let hs = fresh().await;
        // Idempotent: the delete path calls it first, and a retried delete must not fail.
        assert_eq!(hs.revoke_device("never-paired").await.unwrap(), 0);
        let code = hs.issue_pairing_code().await.unwrap().code;
        assert!(pair(&hs, &code, "phone").await.accepted);
        assert_eq!(hs.revoke_device("phone").await.unwrap(), 1);
        assert_eq!(hs.revoke_device("phone").await.unwrap(), 0);
    }

    /// The dashboard's loopback MAC, `HMAC-SHA256(code, challenge || client_id)`; computed
    /// outside this implementation, and it must never change under the desktop app.
    #[test]
    fn the_loopback_mac_is_unchanged() {
        let challenge: Vec<u8> = (0u8..32).collect();
        assert_eq!(
            hex_lower(&pair_mac(b"482915", &challenge, b"install-abcdef").unwrap()),
            "b922abd25f5f1519c9d0f8ccabab4ed17b3e888a3b868be9cfdc320de017ec30",
        );
    }

    /// The dashboard pairs over loopback HTTP with the HMAC exchange; no proof comes back.
    #[tokio::test]
    async fn the_dashboard_still_pairs_over_loopback_hmac() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hs, "dashboard").await;

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "dashboard"),
                device_name: None,
            })
            .await
            .unwrap();
        assert!(resp.accepted, "{:?}", resp.rejection_reason);
        assert_eq!(resp.server_proof, None);
    }

    /// One loopback pairing for `client_id` as `client_type`, with a fresh code.
    async fn loopback_pair(
        hs: &SqliteHandshakeAdapter,
        client_id: &str,
        client_type: &str,
    ) -> HandshakeResponse {
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = hs
            .init_handshake(InitRequest {
                client_id: client_id.into(),
                client_type: client_type.into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, client_id),
                device_name: Some("Pond Desktop".into()),
            })
            .await
            .unwrap();
        assert!(resp.accepted, "{:?}", resp.rejection_reason);
        resp
    }

    /// The dashboard re-pairs under its stored id on every reload that lost its session: the
    /// first pairing admits a device, the later ones re-admit the same row.
    #[tokio::test]
    async fn a_re_pairing_under_a_known_id_is_a_returning_device() {
        let hs = fresh().await;
        assert!(
            !loopback_pair(&hs, "pond-desktop-1", "desktop")
                .await
                .returning_device
        );
        assert!(
            loopback_pair(&hs, "pond-desktop-1", "desktop")
                .await
                .returning_device
        );
        assert!(
            !loopback_pair(&hs, "pond-desktop-2", "desktop")
                .await
                .returning_device,
            "another id is another device"
        );
    }

    /// A known id claimed as another kind of client is not the device the household saw.
    #[tokio::test]
    async fn a_known_id_under_another_client_type_is_new() {
        let hs = fresh().await;
        loopback_pair(&hs, "shared-id", "gotg").await;
        assert!(
            !loopback_pair(&hs, "shared-id", "desktop")
                .await
                .returning_device
        );
    }

    /// CPace reports a return the same way; whether that quiets anything is the route's call.
    #[tokio::test]
    async fn a_cpace_re_pairing_reports_a_returning_device() {
        let hs = fresh().await;
        let first = hs.issue_pairing_code().await.unwrap();
        assert!(
            !pair(&hs, &first.code, "device-again")
                .await
                .returning_device
        );
        let second = hs.issue_pairing_code().await.unwrap();
        let again = pair(&hs, &second.code, "device-again").await;
        assert!(again.accepted, "{:?}", again.rejection_reason);
        assert!(again.returning_device);
    }

    // ---- The printed setup code ------------------------------------------------------------

    use crate::file_setup_code_store::FileSetupCodeStore;
    use pond_core::network::ports::setup_code_store::SetupCodeStore;

    /// A pond with a setup record on disk and the claim service wired, as startup builds it.
    struct SetupPond {
        hs: SqliteHandshakeAdapter,
        store: Arc<FileSetupCodeStore>,
        code: String,
        pool: Pool<Sqlite>,
        dir: tempfile::TempDir,
    }

    async fn setup_pond(members: &[(&str, &str)]) -> SetupPond {
        let dir = tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        for (id, name) in members {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&db.system)
                .await
                .unwrap();
        }
        let store = Arc::new(FileSetupCodeStore::new(dir.path()));
        let record = store.create_if_missing().await.unwrap();
        let hs = SqliteHandshakeAdapter::new(db.system.clone(), Some(OURS.to_string()))
            .with_setup_claims(Arc::new(SetupClaimService::new(store.clone())));
        SetupPond {
            hs,
            store,
            code: record.code.digits().to_string(),
            pool: db.system.clone(),
            dir,
        }
    }

    fn setup_init_request(client_id: &str, nonce: &[u8]) -> PakeInitRequest {
        PakeInitRequest {
            credential: Credential::Setup,
            ..pake_init_request(client_id, OURS, nonce)
        }
    }

    /// The phone's answer under an explicit channel identifier, so a test can pick the label.
    fn answer_under(
        challenge: &PakeChallenge,
        password: &str,
        ci: &[u8],
        client_id: &str,
        nonce: &[u8],
    ) -> (pairing_cpace::Responder, PakeVerifyRequest) {
        let phone = pairing_cpace::Responder::respond(
            password.as_bytes(),
            ci,
            &pairing_cpace::session_id(&B64.decode(&challenge.challenge).unwrap(), nonce),
            &B64.decode(&challenge.share).unwrap(),
        )
        .unwrap();
        let request = PakeVerifyRequest {
            challenge_id: challenge.challenge_id.clone(),
            share: B64.encode(phone.share()),
            confirm: hex_lower(&phone.confirmation()),
            device_name: Some(format!("{client_id} phone")),
        };
        (phone, request)
    }

    /// A whole setup run with `password`, the way goose-on-the-go runs one.
    async fn claim_with(
        hs: &SqliteHandshakeAdapter,
        password: &str,
        client_id: &str,
    ) -> (HandshakeResponse, pairing_cpace::Responder) {
        let nonce = [9u8; 32];
        let challenge = hs
            .init_pake(setup_init_request(client_id, &nonce))
            .await
            .unwrap()
            .expect("an unclaimed pond starts a setup run");
        let ci = pairing_cpace::setup_channel_identifier(OURS, client_id);
        let (phone, request) = answer_under(&challenge, password, &ci, client_id, &nonce);
        (hs.verify_pake(request).await.unwrap(), phone)
    }

    async fn count(pool: &Pool<Sqlite>, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_phone_claims_an_unclaimed_pond_with_its_setup_code() {
        let pond = setup_pond(&[]).await;
        let (resp, phone) = claim_with(&pond.hs, &pond.code, "phone-1").await;

        assert!(resp.accepted, "{:?}", resp.rejection_reason);
        let proof = hex_decode(resp.server_proof.as_deref().unwrap()).unwrap();
        assert!(
            phone.verify_server(&proof),
            "the phone must be able to check the pond"
        );
        assert!(pond
            .hs
            .validate_token(resp.session_token.as_deref().unwrap())
            .await
            .unwrap());
        let record = pond.store.load().await.unwrap().unwrap();
        assert!(
            record.claimed_at.is_some(),
            "the claim is latched by the time the tokens exist"
        );
        let caller = pond
            .hs
            .caller_for_token(resp.session_token.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.claimed_by.as_deref(),
            Some(caller.device_id.as_str()),
            "the claimant is the device the tokens were issued to"
        );
        let (name,): (String,) = sqlx::query_as("SELECT name FROM devices WHERE id = 'phone-1'")
            .fetch_one(&pond.pool)
            .await
            .unwrap();
        assert_eq!(name, "phone-1 phone");
        assert_eq!(
            count(&pond.pool, "pairing_codes").await,
            0,
            "the setup code is not a pairing code"
        );
    }

    #[tokio::test]
    async fn a_household_of_one_owns_the_claiming_phone_and_a_larger_one_does_not() {
        let solo = setup_pond(&[("liz", "Liz")]).await;
        assert!(claim_with(&solo.hs, &solo.code, "phone-1").await.0.accepted);
        let attribution = SqliteDeviceAttribution::new(solo.pool.clone());
        assert_eq!(
            attribution.device_profile("phone-1").await.unwrap(),
            Some("liz".to_string())
        );

        let pair_of = setup_pond(&[("liz", "Liz"), ("jerry", "Jerry")]).await;
        assert!(
            claim_with(&pair_of.hs, &pair_of.code, "phone-2")
                .await
                .0
                .accepted
        );
        let attribution = SqliteDeviceAttribution::new(pair_of.pool.clone());
        assert_eq!(attribution.device_profile("phone-2").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_claimed_pond_refuses_its_setup_code_at_init() {
        let pond = setup_pond(&[]).await;
        pond.store.mark_claimed(Utc::now(), None).await.unwrap();
        let refusal = pond
            .hs
            .init_pake(setup_init_request("phone-1", &[1u8; 32]))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(refusal, PakeRefusal::SetupCodeUnavailable);
        assert_eq!(refusal.code(), "setup_code_unavailable");
    }

    #[tokio::test]
    async fn a_pond_with_no_record_or_no_claim_service_refuses_the_setup_code() {
        let unwired = fresh().await;
        assert_eq!(
            unwired
                .init_pake(setup_init_request("phone-1", &[1u8; 32]))
                .await
                .unwrap()
                .unwrap_err(),
            PakeRefusal::SetupCodeUnavailable
        );

        let dir = tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        let empty = Arc::new(FileSetupCodeStore::new(dir.path()));
        let hs = SqliteHandshakeAdapter::new(db.system.clone(), Some(OURS.to_string()))
            .with_setup_claims(Arc::new(SetupClaimService::new(empty)));
        assert_eq!(
            hs.init_pake(setup_init_request("phone-1", &[1u8; 32]))
                .await
                .unwrap()
                .unwrap_err(),
            PakeRefusal::SetupCodeUnavailable
        );
    }

    #[tokio::test]
    async fn the_sixth_setup_attempt_cools_and_says_how_long_to_wait() {
        let pond = setup_pond(&[]).await;
        for attempt in 0..pond_core::network::services::setup_guess_throttle::FREE_SETUP_FAILURES {
            assert!(
                pond.hs
                    .init_pake(setup_init_request("phone-1", &[attempt as u8; 32]))
                    .await
                    .unwrap()
                    .is_ok(),
                "attempt {attempt} is free"
            );
        }
        let refusal = pond
            .hs
            .init_pake(setup_init_request("phone-1", &[7u8; 32]))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(refusal.code(), "setup_code_cooling");
        let wait = refusal
            .retry_after()
            .expect("cooling says how long")
            .as_secs_ceil();
        assert!((1..=30).contains(&wait), "waited {wait} s");
    }

    #[tokio::test]
    async fn a_pond_claimed_between_init_and_verify_mints_nothing() {
        let pond = setup_pond(&[]).await;
        let nonce = [3u8; 32];
        let challenge = pond
            .hs
            .init_pake(setup_init_request("late-phone", &nonce))
            .await
            .unwrap()
            .unwrap();
        // Another device claims while this phone is still answering.
        pond.store.mark_claimed(Utc::now(), None).await.unwrap();

        let ci = pairing_cpace::setup_channel_identifier(OURS, "late-phone");
        let (_, request) = answer_under(&challenge, &pond.code, &ci, "late-phone", &nonce);
        let resp = pond.hs.verify_pake(request).await.unwrap();
        assert!(!resp.accepted);
        assert_eq!(
            resp.rejection_reason.as_deref(),
            Some("setup_code_unavailable")
        );
        assert!(resp.session_token.is_none() && resp.refresh_token.is_none());
        assert!(resp.server_proof.is_none(), "no proof without a claim");
        assert_eq!(count(&pond.pool, "session_tokens").await, 0);
        assert_eq!(count(&pond.pool, "devices").await, 0);
    }

    #[tokio::test]
    async fn two_phones_racing_the_card_leave_exactly_one_paired() {
        let pond = setup_pond(&[]).await;
        let mut runs = Vec::new();
        for (client, nonce) in [("phone-a", [1u8; 32]), ("phone-b", [2u8; 32])] {
            let challenge = pond
                .hs
                .init_pake(setup_init_request(client, &nonce))
                .await
                .unwrap()
                .unwrap();
            let ci = pairing_cpace::setup_channel_identifier(OURS, client);
            runs.push(answer_under(&challenge, &pond.code, &ci, client, &nonce).1);
        }
        let second = runs.pop().unwrap();
        let first = runs.pop().unwrap();
        let (a, b) = tokio::join!(pond.hs.verify_pake(first), pond.hs.verify_pake(second));
        let outcomes = [a.unwrap(), b.unwrap()];
        assert_eq!(outcomes.iter().filter(|r| r.accepted).count(), 1);
        let loser = outcomes.iter().find(|r| !r.accepted).unwrap();
        assert_eq!(
            loser.rejection_reason.as_deref(),
            Some("setup_code_unavailable")
        );
        assert_eq!(count(&pond.pool, "devices").await, 1);
        let winner = outcomes.iter().find(|r| r.accepted).unwrap();
        let caller = pond
            .hs
            .caller_for_token(winner.session_token.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pond.store.load().await.unwrap().unwrap().claimed_by,
            Some(caller.device_id),
            "the record names the phone that won, not the one that lost"
        );
    }

    #[tokio::test]
    async fn a_phone_answering_under_the_pairing_label_fails_a_setup_run() {
        let pond = setup_pond(&[]).await;
        let nonce = [4u8; 32];
        let challenge = pond
            .hs
            .init_pake(setup_init_request("phone-1", &nonce))
            .await
            .unwrap()
            .unwrap();
        let pairing_ci = pairing_cpace::channel_identifier(OURS, "phone-1");
        let (_, request) = answer_under(&challenge, &pond.code, &pairing_ci, "phone-1", &nonce);
        let resp = pond.hs.verify_pake(request).await.unwrap();
        assert!(!resp.accepted);
        assert_eq!(
            resp.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        assert_eq!(
            pond.store.load().await.unwrap().unwrap().claimed_at,
            None,
            "a failed run must not claim"
        );
    }

    #[tokio::test]
    async fn a_failed_setup_run_leaves_pairing_codes_and_their_budgets_untouched() {
        let pond = setup_pond(&[]).await;
        let pc = pond.hs.issue_pairing_code().await.unwrap();
        let code_hash = SqliteHandshakeAdapter::sha256_hex(pc.code.as_bytes());

        for attempt in 0..pond_core::network::services::setup_guess_throttle::FREE_SETUP_FAILURES {
            let (resp, _) = claim_with(&pond.hs, "00000000000", "guesser").await;
            assert!(!resp.accepted);
            assert_eq!(
                resp.rejection_reason.as_deref(),
                Some("invalid_confirmation"),
                "attempt {attempt}: a setup run never answers code_retired"
            );
        }
        assert_eq!(pond.hs.pake_state().attempts.get(&code_hash), None);
        assert!(pond
            .hs
            .active_code_profile(&code_hash)
            .await
            .unwrap()
            .is_some());
        assert!(pair(&pond.hs, &pc.code, "phone-1").await.accepted);
    }

    #[tokio::test]
    async fn the_pond_remembers_which_secret_each_waiting_run_holds() {
        let pond = setup_pond(&[]).await;
        pond.hs.issue_pairing_code().await.unwrap();
        let setup = pond
            .hs
            .init_pake(setup_init_request("phone-1", &[1u8; 32]))
            .await
            .unwrap()
            .unwrap();
        let pairing = pake_init(&pond.hs, "phone-2", &[2u8; 32]).await;
        assert_eq!(
            pond.hs.pake_credential(&setup.challenge_id).await,
            Some(Credential::Setup)
        );
        assert_eq!(
            pond.hs.pake_credential(&pairing.challenge_id).await,
            Some(Credential::Pairing)
        );
        assert_eq!(pond.hs.pake_credential("no-such-run").await, None);
    }

    #[tokio::test]
    async fn every_pairing_code_path_latches_the_claim() {
        let claimed = |pond: &SetupPond| {
            let store = pond.store.clone();
            async move { store.load().await.unwrap().unwrap().claimed_at.is_some() }
        };
        let claimant = |pond: &SetupPond| {
            let store = pond.store.clone();
            async move { store.load().await.unwrap().unwrap().claimed_by }
        };

        let cpace = setup_pond(&[]).await;
        let pc = cpace.hs.issue_pairing_code().await.unwrap();
        assert!(!claimed(&cpace).await);
        assert!(pair(&cpace.hs, &pc.code, "phone").await.accepted);
        assert!(claimed(&cpace).await, "CPace with a pairing code");
        assert_eq!(claimant(&cpace).await.as_deref(), Some("phone"));
        let later = cpace.hs.issue_pairing_code().await.unwrap();
        assert!(pair(&cpace.hs, &later.code, "second-phone").await.accepted);
        assert_eq!(
            claimant(&cpace).await.as_deref(),
            Some("phone"),
            "a later pairing never takes the claim over"
        );
        assert_eq!(
            cpace
                .hs
                .init_pake(setup_init_request("phone-2", &[1u8; 32]))
                .await
                .unwrap()
                .unwrap_err(),
            PakeRefusal::SetupCodeUnavailable,
            "once claimed, the card is refused"
        );

        let hmac = setup_pond(&[]).await;
        let pc = hmac.hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hmac.hs, "dashboard").await;
        let resp = hmac
            .hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "dashboard"),
                device_name: None,
            })
            .await
            .unwrap();
        assert!(resp.accepted);
        assert!(claimed(&hmac).await, "loopback HMAC");
        assert_eq!(claimant(&hmac).await.as_deref(), Some("dashboard"));

        let legacy = setup_pond(&[]).await;
        let pc = legacy.hs.issue_pairing_code().await.unwrap();
        let resp = legacy
            .hs
            .handshake(HandshakeRequest {
                client_id: "cli".into(),
                client_type: "cli".into(),
                client_version: "1.0".into(),
                pairing_code: Some(pc.code),
            })
            .await
            .unwrap();
        assert!(resp.accepted);
        assert!(claimed(&legacy).await, "legacy single step");
        assert_eq!(claimant(&legacy).await.as_deref(), Some("cli"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_broken_setup_directory_does_not_stop_a_pairing() {
        use std::os::unix::fs::PermissionsExt;
        let pond = setup_pond(&[]).await;
        let setup_dir = pond
            .dir
            .path()
            .join(crate::file_setup_code_store::SETUP_DIR);
        std::fs::set_permissions(&setup_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let pc = pond.hs.issue_pairing_code().await.unwrap();
        assert!(
            pair(&pond.hs, &pc.code, "phone").await.accepted,
            "a setup store that cannot be read must not take pairing down"
        );
        assert_eq!(
            pond.hs
                .init_pake(setup_init_request("phone-2", &[1u8; 32]))
                .await
                .unwrap()
                .unwrap_err(),
            PakeRefusal::SetupCodeUnavailable,
            "and the card fails closed"
        );
    }

    // ---- The setup channel: the same exchange in process, with keys ---------------------

    async fn channel_setup_run(
        pond: &SetupPond,
        password: &str,
        client_id: &str,
    ) -> (ChannelVerify, pairing_cpace::Responder) {
        let nonce = [4u8; 32];
        let challenge = pond
            .hs
            .init_channel_pake(setup_init_request(client_id, &nonce))
            .await
            .unwrap()
            .expect("an unclaimed pond starts a setup run on the channel");
        let ci = pairing_cpace::setup_channel_identifier(OURS, client_id);
        let (phone, request) = answer_under(&challenge, password, &ci, client_id, &nonce);
        (pond.hs.verify_channel_pake(request).await.unwrap(), phone)
    }

    #[tokio::test]
    async fn an_accepted_channel_run_hands_over_the_keys_the_phone_derives() {
        let pond = setup_pond(&[]).await;
        let (verified, phone) = channel_setup_run(&pond, &pond.code, "ble-phone").await;
        assert!(
            verified.response.accepted,
            "{:?}",
            verified.response.rejection_reason
        );
        let proof = hex_decode(verified.response.server_proof.as_deref().unwrap()).unwrap();
        assert!(phone.verify_server(&proof));

        let keys = verified
            .keys
            .expect("an accepted channel run carries its keys");
        let expected = phone.channel_keys();
        assert_eq!(keys.pond_to_phone(), expected.pond_to_phone());
        assert_eq!(keys.phone_to_pond(), expected.phone_to_pond());

        assert!(pond
            .store
            .load()
            .await
            .unwrap()
            .unwrap()
            .claim_state()
            .is_claimed());
        let token = verified.response.session_token.unwrap();
        assert!(pond.hs.validate_token(&token).await.unwrap());
    }

    #[tokio::test]
    async fn a_refused_channel_run_has_no_keys_and_spends_what_https_would() {
        let pond = setup_pond(&[]).await;
        let (verified, _) = channel_setup_run(&pond, "34970112330", "ble-phone").await;
        assert!(!verified.response.accepted);
        assert_eq!(
            verified.response.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        assert!(verified.keys.is_none());
        assert!(!pond
            .store
            .load()
            .await
            .unwrap()
            .unwrap()
            .claim_state()
            .is_claimed());
    }

    #[tokio::test]
    async fn a_channel_pairing_run_consumes_the_code_and_claims_like_https() {
        let pond = setup_pond(&[]).await;
        let pc = pond.hs.issue_pairing_code().await.unwrap();
        let nonce = [6u8; 32];
        let challenge = pond
            .hs
            .init_channel_pake(pake_init_request("ble-phone", OURS, &nonce))
            .await
            .unwrap()
            .unwrap();
        let (phone, request) = pake_answer(&challenge, &pc.code, "ble-phone", OURS, &nonce);
        let verified = pond.hs.verify_channel_pake(request).await.unwrap();
        assert!(verified.response.accepted);
        assert_eq!(
            verified.keys.unwrap().phone_to_pond(),
            phone.channel_keys().phone_to_pond()
        );
        assert!(pond
            .store
            .load()
            .await
            .unwrap()
            .unwrap()
            .claim_state()
            .is_claimed());
        assert!(
            pond.hs.current_pairing_code().await.unwrap().is_none(),
            "the code is spent"
        );
    }

    /// A BLE challenge id travels in the clear, so a verify over HTTPS that names it must not
    /// burn the phone's run; and an HTTPS run never yields keys on the channel.
    #[tokio::test]
    async fn a_run_is_finished_only_on_the_transport_that_started_it() {
        let pond = setup_pond(&[]).await;
        let nonce = [4u8; 32];
        let challenge = pond
            .hs
            .init_channel_pake(setup_init_request("ble-phone", &nonce))
            .await
            .unwrap()
            .unwrap();
        let ci = pairing_cpace::setup_channel_identifier(OURS, "ble-phone");
        let (_, request) = answer_under(&challenge, &pond.code, &ci, "ble-phone", &nonce);
        let over_https = pond.hs.verify_pake(request.clone()).await.unwrap();
        assert!(!over_https.accepted);
        assert_eq!(
            over_https.rejection_reason.as_deref(),
            Some("unknown_challenge")
        );
        assert!(
            pond.hs
                .verify_channel_pake(request)
                .await
                .unwrap()
                .response
                .accepted,
            "the channel run was left alone"
        );

        let pc = pond.hs.issue_pairing_code().await.unwrap();
        let challenge = pake_init(&pond.hs, "lan-phone", &nonce).await;
        let (_, request) = pake_answer(&challenge, &pc.code, "lan-phone", OURS, &nonce);
        let on_channel = pond.hs.verify_channel_pake(request.clone()).await.unwrap();
        assert!(!on_channel.response.accepted);
        assert!(on_channel.keys.is_none());
        assert!(
            pond.hs.verify_pake(request).await.unwrap().accepted,
            "the HTTPS run was left alone"
        );
    }

    #[tokio::test]
    async fn a_claimed_pond_refuses_the_card_on_the_channel_too() {
        let pond = setup_pond(&[]).await;
        let (first, _) = channel_setup_run(&pond, &pond.code, "ble-phone").await;
        assert!(first.response.accepted);
        assert_eq!(
            pond.hs
                .init_channel_pake(setup_init_request("ble-phone-2", &[1u8; 32]))
                .await
                .unwrap()
                .unwrap_err(),
            PakeRefusal::SetupCodeUnavailable
        );
    }

    // ---- A paired device resuming on the setup channel ----------------------------------

    use pond_core::network::services::device_resume_throttle::{
        DEVICE_RESUME_ATTEMPTS, DEVICE_RESUME_WINDOW,
    };
    use std::time::Instant;

    fn device_init_request(client_id: &str, nonce: &[u8]) -> PakeInitRequest {
        PakeInitRequest {
            credential: Credential::Device,
            ..pake_init_request(client_id, OURS, nonce)
        }
    }

    async fn start_device_run(
        hs: &SqliteHandshakeAdapter,
        client_id: &str,
        nonce: &[u8],
    ) -> std::result::Result<PakeChallenge, PakeRefusal> {
        hs.init_channel_pake(device_init_request(client_id, nonce))
            .await
            .unwrap()
    }

    /// The phone's answer to a device run, proving it holds `refresh_token`.
    fn device_answer(
        challenge: &PakeChallenge,
        refresh_token: &str,
        client_id: &str,
        nonce: &[u8],
    ) -> (pairing_cpace::Responder, PakeVerifyRequest) {
        let password = pairing_cpace::device_password(refresh_token);
        let ci = pairing_cpace::device_channel_identifier(OURS, client_id);
        answer_under(challenge, &password, &ci, client_id, nonce)
    }

    /// A whole device run, the way goose-on-the-go resumes over BLE.
    async fn resume(
        hs: &SqliteHandshakeAdapter,
        refresh_token: &str,
        client_id: &str,
    ) -> (ChannelVerify, pairing_cpace::Responder) {
        let nonce = [0x21u8; 32];
        let challenge = start_device_run(hs, client_id, &nonce)
            .await
            .expect("the pond starts a device run");
        let (phone, request) = device_answer(&challenge, refresh_token, client_id, &nonce);
        (hs.verify_channel_pake(request).await.unwrap(), phone)
    }

    /// The password a waiting device run committed to.
    fn committed_password(hs: &SqliteHandshakeAdapter, challenge_id: &str) -> String {
        match &hs
            .pake_state()
            .pending
            .get(challenge_id)
            .expect("a waiting run")
            .secret
        {
            RunSecret::Device { refresh_hash } => refresh_hash.to_string(),
            _ => panic!("a device run holds a device secret"),
        }
    }

    async fn active_sessions(pool: &Pool<Sqlite>, device: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_tokens WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind(device)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_paired_device_resumes_with_a_rotated_session_and_keeps_its_owner() {
        let (hs, pool) = fresh_with_household().await;
        let pc = hs.issue_pairing_code_for(Some("liz")).await.unwrap();
        let paired = pair(&hs, &pc.code, "liz-phone").await;
        assert!(paired.accepted);
        let old_session = paired.session_token.unwrap();
        let old_refresh = paired.refresh_token.unwrap();

        let nonce = [0x31u8; 32];
        let challenge = start_device_run(&hs, "liz-phone", &nonce).await.unwrap();
        assert_eq!(
            hs.pake_credential(&challenge.challenge_id).await,
            Some(Credential::Device)
        );
        let (phone, mut request) = device_answer(&challenge, &old_refresh, "liz-phone", &nonce);
        request.device_name = Some("Somebody else's phone".into());
        let verified = hs.verify_channel_pake(request).await.unwrap();

        let response = &verified.response;
        assert!(response.accepted, "{:?}", response.rejection_reason);
        let proof = hex_decode(response.server_proof.as_deref().unwrap()).unwrap();
        assert!(phone.verify_server(&proof), "the phone can check the pond");
        let keys = verified
            .keys
            .as_ref()
            .expect("a device run seals the channel");
        assert_eq!(keys.pond_to_phone(), phone.channel_keys().pond_to_phone());
        assert_eq!(keys.phone_to_pond(), phone.channel_keys().phone_to_pond());

        let new_session = response.session_token.clone().unwrap();
        let new_refresh = response.refresh_token.clone().unwrap();
        assert_ne!(new_refresh, old_refresh);
        assert!(hs.validate_token(&new_session).await.unwrap());
        assert!(
            !hs.validate_token(&old_session).await.unwrap(),
            "the old session went with its refresh row"
        );
        let replay = hs
            .refresh(RefreshRequest {
                refresh_token: old_refresh,
            })
            .await
            .unwrap();
        assert_eq!(
            replay.rejection_reason.as_deref(),
            Some("invalid_or_expired_refresh")
        );
        assert_eq!(active_sessions(&pool, "liz-phone").await, 1);
        assert_eq!(
            hs.caller_for_token(&new_session).await.unwrap(),
            Some(TokenCaller {
                client_id: "liz-phone".into(),
                device_id: "liz-phone".into(),
            })
        );

        let attribution = SqliteDeviceAttribution::new(pool.clone());
        assert_eq!(
            attribution.device_profile("liz-phone").await.unwrap(),
            Some("liz".to_string()),
            "a resume keeps the device's member"
        );
        let (name,): (String,) = sqlx::query_as("SELECT name FROM devices WHERE id = 'liz-phone'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            name, "liz-phone phone",
            "the run's device_name renames nothing"
        );
    }

    #[tokio::test]
    async fn a_device_run_neither_latches_the_claim_nor_needs_a_code() {
        let pond = setup_pond(&[]).await;
        let (_, refresh, _) = pond
            .hs
            .issue_session_pair("phone", "gotg", "phone")
            .await
            .unwrap();
        assert!(pond.hs.current_pairing_code().await.unwrap().is_none());

        let (verified, _) = resume(&pond.hs, &refresh, "phone").await;
        assert!(
            verified.response.accepted,
            "{:?}",
            verified.response.rejection_reason
        );
        assert!(
            !pond
                .store
                .load()
                .await
                .unwrap()
                .unwrap()
                .claim_state()
                .is_claimed(),
            "a resume is not a first pairing"
        );
        assert_eq!(count(&pond.pool, "pairing_codes").await, 0);
        assert_eq!(
            count(&pond.pool, "devices").await,
            0,
            "no devices row is written"
        );
    }

    #[tokio::test]
    async fn a_rotated_refresh_token_fails_and_the_current_one_resumes() {
        let hs = fresh().await;
        let paired = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        let stale = paired.refresh_token.unwrap();
        let current = hs
            .refresh(RefreshRequest {
                refresh_token: stale.clone(),
            })
            .await
            .unwrap()
            .refresh_token
            .unwrap();

        let (verified, _) = resume(&hs, &stale, "phone").await;
        assert_eq!(
            verified.response.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        assert!(verified.keys.is_none());
        assert!(verified.response.session_token.is_none());
        assert!(verified.response.server_proof.is_none());

        let (verified, _) = resume(&hs, &current, "phone").await;
        assert!(verified.response.accepted);
    }

    /// From outside, an id with no session and a paired id with the wrong token are one case:
    /// both runs commit to 64 hex characters, both charge the throttle, and both are refused at
    /// confirmation with the same answer.
    #[tokio::test]
    async fn an_unknown_id_takes_the_same_path_to_invalid_confirmation_as_a_wrong_token() {
        let hs = fresh().await;
        let paired = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        let stored =
            SqliteHandshakeAdapter::sha256_hex(paired.refresh_token.as_deref().unwrap().as_bytes());

        let nonce = [0x42u8; 32];
        let known = start_device_run(&hs, "phone", &nonce).await.unwrap();
        let unknown = start_device_run(&hs, "stranger", &nonce).await.unwrap();
        let unknown_again = start_device_run(&hs, "stranger", &nonce).await.unwrap();
        assert_eq!(committed_password(&hs, &known.challenge_id), stored);
        let stand_in = committed_password(&hs, &unknown.challenge_id);
        for password in [&stored, &stand_in] {
            assert_eq!(password.len(), 64);
            assert!(password
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        }
        assert_ne!(
            stand_in,
            committed_password(&hs, &unknown_again.challenge_id),
            "each run gets a fresh stand-in"
        );
        let (matching,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM session_tokens WHERE refresh_hash = ?")
                .bind(&stand_in)
                .fetch_one(&hs.pool)
                .await
                .unwrap();
        assert_eq!(matching, 0, "the stand-in names no session");
        for challenge in [&known, &unknown] {
            assert_eq!(challenge.suite, pairing_cpace::SUITE);
            assert_eq!(B64.decode(&challenge.share).unwrap().len(), 32);
        }

        let (_, wrong) = device_answer(&known, "not-the-token", "phone", &nonce);
        let (_, guess) = device_answer(&unknown, "not-the-token", "stranger", &nonce);
        let wrong = hs.verify_channel_pake(wrong).await.unwrap();
        let guess = hs.verify_channel_pake(guess).await.unwrap();
        assert_eq!(
            wrong.response.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        assert_eq!(
            serde_json::to_value(&wrong.response).unwrap(),
            serde_json::to_value(&guess.response).unwrap()
        );
        assert!(wrong.keys.is_none() && guess.keys.is_none());
        let now = Instant::now();
        assert_eq!(hs.pake_state().device_resumes.charged("phone", now), 1);
        assert_eq!(hs.pake_state().device_resumes.charged("stranger", now), 2);
    }

    #[tokio::test]
    async fn five_failed_device_runs_cool_that_device_and_a_success_resets_its_count() {
        let hs = fresh().await;
        let paired = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        let token = paired.refresh_token.unwrap();

        for _ in 0..DEVICE_RESUME_ATTEMPTS - 1 {
            let (failed, _) = resume(&hs, "not-the-token", "phone").await;
            assert_eq!(
                failed.response.rejection_reason.as_deref(),
                Some("invalid_confirmation")
            );
        }
        let (resumed, _) = resume(&hs, &token, "phone").await;
        assert!(resumed.response.accepted, "the fifth run is still allowed");
        let session = resumed.response.session_token.clone().unwrap();
        assert_eq!(
            hs.pake_state()
                .device_resumes
                .charged("phone", Instant::now()),
            0,
            "a success resets the count"
        );

        for id in ["phone", "stranger"] {
            for _ in 0..DEVICE_RESUME_ATTEMPTS {
                let (failed, _) = resume(&hs, "not-the-token", id).await;
                assert_eq!(
                    failed.response.rejection_reason.as_deref(),
                    Some("invalid_confirmation"),
                    "{id}"
                );
            }
            let refusal = start_device_run(&hs, id, &[1u8; 32]).await.unwrap_err();
            assert_eq!(refusal.code(), "device_cooling", "{id}");
            let wait = refusal
                .retry_after()
                .expect("cooling says how long")
                .as_secs_ceil();
            assert!(
                (1..=DEVICE_RESUME_WINDOW.as_secs()).contains(&wait),
                "{id} waits {wait} s"
            );
        }
        assert!(
            start_device_run(&hs, "tablet", &[1u8; 32]).await.is_ok(),
            "each device cools alone"
        );
        assert!(
            hs.validate_token(&session).await.unwrap(),
            "failed runs leave the device's session alone"
        );
    }

    /// A device removed from the household has no active row, so it meets the stand-in.
    #[tokio::test]
    async fn a_revoked_device_cannot_resume() {
        let hs = fresh().await;
        let paired = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        assert_eq!(hs.revoke_device("phone").await.unwrap(), 1);
        let (verified, _) = resume(&hs, paired.refresh_token.as_deref().unwrap(), "phone").await;
        assert_eq!(
            verified.response.rejection_reason.as_deref(),
            Some("invalid_confirmation")
        );
        assert_eq!(active_sessions(&hs.pool, "phone").await, 0);
    }

    #[tokio::test]
    async fn https_refuses_the_device_credential_before_anything_is_spent() {
        let hs = fresh().await;
        let paired = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        let refusal = hs
            .init_pake(device_init_request("phone", &[1u8; 32]))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(refusal, PakeRefusal::InvalidCredential);
        assert_eq!(refusal.code(), "invalid_credential");
        assert_eq!(
            hs.pake_state()
                .device_resumes
                .charged("phone", Instant::now()),
            0
        );
        assert!(hs.pake_state().pending.is_empty(), "no run was started");
        let refreshed = hs
            .refresh(RefreshRequest {
                refresh_token: paired.refresh_token.unwrap(),
            })
            .await
            .unwrap();
        assert!(refreshed.accepted, "HTTPS has refresh for this");
    }

    #[tokio::test]
    async fn a_device_run_whose_session_rotated_meanwhile_mints_nothing() {
        let hs = fresh().await;
        let paired = pair(&hs, &hs.issue_pairing_code().await.unwrap().code, "phone").await;
        let token = paired.refresh_token.unwrap();
        let nonce = [0x17u8; 32];
        let challenge = start_device_run(&hs, "phone", &nonce).await.unwrap();
        let rotated = hs
            .refresh(RefreshRequest {
                refresh_token: token.clone(),
            })
            .await
            .unwrap();

        let (_, request) = device_answer(&challenge, &token, "phone", &nonce);
        let verified = hs.verify_channel_pake(request).await.unwrap();
        assert_eq!(
            verified.response.rejection_reason.as_deref(),
            Some("invalid_or_expired_refresh")
        );
        assert!(verified.keys.is_none());
        assert!(verified.response.server_proof.is_none());
        assert!(
            hs.validate_token(rotated.session_token.as_deref().unwrap())
                .await
                .unwrap(),
            "the rotation that won stands"
        );
        assert_eq!(active_sessions(&hs.pool, "phone").await, 1);
    }
}
