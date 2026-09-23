//! SQLite-backed [`Handshake`] adapter implementing the two-phase pairing
//! protocol from `pond-core::ports::handshake` (#93).
//!
//! Tables (see `migrations/system/0023_handshake.sql`):
//! - `pairing_codes`        — single-use codes (only sha256 hash persisted)
//! - `handshake_challenges` — short-lived challenges, one per `init`
//! - `session_tokens`       — issued session + refresh tokens (sha256-hashed)
//!
//! Security properties:
//! - Tokens returned to clients are opaque base64 of 32 random bytes; only
//!   sha256 hashes are ever stored.
//! - MAC and hash comparisons are constant-time (`subtle`).
//! - Pairing codes are never sent over the wire — the client uses the code as
//!   an HMAC key, and the server only ever sees the resulting MAC.

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
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use pond_core::security::ports::handshake::{
    ChallengeResponse, Handshake, HandshakeRequest, HandshakeResponse, InitRequest, PairingCode,
    RefreshRequest, TokenCaller, VerifyRequest,
};
use pond_core::user_data::ports::device_attribution::checked_profile_id;

const PAIRING_CODE_TTL_MIN: i64 = 10;
const CHALLENGE_TTL_SEC: i64 = 60;
const SESSION_TTL_HOURS: i64 = 24;
const REFRESH_TTL_DAYS: i64 = 30;

type HmacSha256 = Hmac<Sha256>;

/// Plaintext pairing codes are kept **process-local**: the operator reads them
/// off this server's stdout/dashboard, and clients must pair before the process
/// restarts. On restart, unused codes from a prior run are silently invalidated
/// (no plaintext is available to verify against), which matches the operator
/// workflow of reading the code displayed at *this* startup.
static ISSUED_CODE_CACHE: Lazy<RwLock<HashMap<String, String>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// SQLite-backed implementation of the [`Handshake`] port.
pub struct SqliteHandshakeAdapter {
    pool: Pool<Sqlite>,
    hostname: String,
    server_version: String,
    capabilities: Vec<String>,
    spki_pin: Option<String>,
}

impl SqliteHandshakeAdapter {
    /// `spki_pin` is this server's own public-key pin, in `sha256/<base64>`
    /// form, and it is what a client's channel binding is checked against.
    ///
    /// It is a required argument rather than a builder step because a forgotten
    /// builder step would leave channel binding inert -- pairing would keep
    /// working, and nothing would notice that the property had gone. A caller
    /// with no TLS identity to name says so with `None`, which rejects any
    /// client that claims a binding rather than ignoring it.
    pub fn new(pool: Pool<Sqlite>, spki_pin: Option<String>) -> Self {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        Self {
            pool,
            hostname,
            spki_pin,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![
                "chat".to_string(),
                "devices".to_string(),
                "settings".to_string(),
            ],
        }
    }

    /// Wrap as `Arc<dyn Handshake>`.
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

    /// Opaque session/refresh token: base64 of 32 random bytes.
    fn random_token() -> String {
        B64.encode(Self::random_bytes(32))
    }

    /// 6-digit numeric pairing code, leading zeros preserved.
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
            // Set only by the bound branch of `verify_handshake`; every other
            // response has no transcript to prove anything over.
            server_proof: None,
        }
    }

    fn reject(&self, reason: &str) -> HandshakeResponse {
        self.response(false, None, None, None, Some(reason.to_string()))
    }

    /// Mint a new session+refresh pair, persist the hashes, and return the
    /// plaintext to the caller.
    ///
    /// Side effect: revokes any previously-active session for the same
    /// `device_id`, so re-pairing from the same install supersedes rather than
    /// accumulates orphaned tokens.
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

    /// Match a plaintext pairing code against active (unconsumed, unexpired)
    /// rows. Returns the matched `code_hash`.
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

        // PAI-1: this path writes no `devices` row at all, so there is nothing
        // to attribute and a legacy pair is always unattributed. That is the
        // narrowing outcome and it is left alone deliberately -- registering a
        // device here would be a behaviour change outside this rung. The wart
        // worth knowing about is that a member-bound code consumed here is
        // BURNED with its attribution discarded; the operator re-issues.
        let device_id = request.client_id.clone();
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

    /// Same predicate as [`validate_token`](Self::validate_token) — unexpired
    /// and unrevoked — but returns who the token was issued to rather than a
    /// bool, so an authenticated request can name its caller and its device.
    /// Deliberately does NOT touch `last_seen_at`: this is a lookup about a
    /// request already validated, not a second use of the token.
    ///
    /// PAI-1 P9: `device_id` is the row `verify_handshake` registered in
    /// `devices`, and `devices.profile_id` is the member the operator bound at
    /// pairing-code issuance. Selecting it here is the only way that member
    /// reaches a turn — the alternative, a client-supplied device id, would
    /// outrank every proof the pond can make.
    ///
    /// The predicate is repeated rather than shared with `validate_token`
    /// because the two answer different questions about the same row and
    /// `validate_token` has the `last_seen_at` side effect. If they ever
    /// disagree, this one is the stricter place to fix: a token that validates
    /// but names nobody degrades to `<unknown>`, which is a narrowing.
    ///
    /// [`client_id_for_token`](Handshake::client_id_for_token) is NOT
    /// overridden: the port derives it from this method so the two cannot drift
    /// into disagreeing, and so an adapter that loses this override visibly
    /// loses the client id as well.
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
        // Marked rather than deleted, like every other revocation here: the row
        // is what `validate_token` reads, and a deleted row and an unknown
        // token are indistinguishable to an auditor later.
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
        // The route has already authenticated this token. A refresh may have rotated
        // its row while durable network revocation was being queued. Revoke the
        // device selected by that authenticated row, including any completed rotation.
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
        // 0. A client that names the key it pinned must have pinned THIS
        //    server's. Checked before the challenge is consumed, so a client
        //    that reached the wrong pond can simply start again once it has
        //    the right pin; and checked at all, because a binding the server
        //    ignored would be a binding an interceptor could strip.
        if let Some(claimed) = request.channel_binding.as_deref() {
            let ours = self.spki_pin.as_deref();
            if ours != Some(claimed) {
                tracing::warn!(
                    claimed,
                    ours = ours.unwrap_or("<none: this server has no TLS identity>"),
                    "pairing refused: the client pinned a key that is not this pond's"
                );
                return Ok(self.reject("channel_binding_mismatch"));
            }
        }

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

        // 2. Consume the challenge up front — one challenge authorizes exactly
        //    one verify attempt, success or failure. A wrong guess therefore
        //    burns the challenge (the client must `init` again, which is
        //    rate-limited per IP) and never touches the operator's pairing code,
        //    so there is no remote-triggerable lockout. The `consumed_at IS NULL`
        //    guard also closes the read→update race if two requests race on the
        //    same challenge.
        let attempt_at = Utc::now().to_rfc3339();
        let consumed = sqlx::query(
            "UPDATE handshake_challenges SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL",
        )
        .bind(&attempt_at)
        .bind(&request.challenge_id)
        .execute(&self.pool)
        .await?;
        if consumed.rows_affected() == 0 {
            return Ok(self.reject("challenge_used"));
        }

        // 3. Find the active pairing code whose plaintext (process-local)
        //    reproduces the submitted MAC.
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

        // The plaintext rides out with the hash: the server proof below is
        // keyed by the same code, and re-reading the cache to find it again
        // would be a second chance to pick a different one.
        let mut matched: Option<(String, String)> = None;
        {
            let cache = ISSUED_CODE_CACHE.read().await;
            for (code_hash,) in &codes {
                if let Some(plaintext) = cache.get(code_hash) {
                    if verify_mac(
                        plaintext.as_bytes(),
                        &challenge,
                        client_id.as_bytes(),
                        request.channel_binding.as_deref(),
                        &mac_bytes,
                    ) {
                        matched = Some((code_hash.clone(), plaintext.clone()));
                        break;
                    }
                }
            }
        }

        let Some((code_hash, code_plaintext)) = matched else {
            // Wrong code/MAC. The challenge is already spent (step 2) and the
            // pairing code is untouched, so a bad guess can neither be retried
            // on this challenge nor lock the operator out.
            return Ok(self.reject("invalid_mac"));
        };

        // 4. Consume the matched code (single-use on success), register the
        //    device, then mint tokens.
        //
        //    PAI-1: the code carries the household member the operator issued it
        //    for, and that is the ONLY thing that attributes the device. Nothing
        //    in `request` is consulted, deliberately -- see
        //    `Handshake::issue_pairing_code_for` for why a client-asserted
        //    profile would outrank every proof the pond can make.
        let code_profile: Option<String> =
            sqlx::query_scalar("SELECT profile_id FROM pairing_codes WHERE code_hash = ?")
                .bind(&code_hash)
                .fetch_optional(&self.pool)
                .await?
                .flatten();

        sqlx::query("UPDATE pairing_codes SET consumed_at = ? WHERE code_hash = ?")
            .bind(&attempt_at)
            .bind(&code_hash)
            .execute(&self.pool)
            .await?;
        ISSUED_CODE_CACHE.write().await.remove(&code_hash);

        let device_id = client_id.clone();
        let device_name = request
            .device_name
            .clone()
            .unwrap_or_else(|| format!("gotg-{}", client_id.chars().take(8).collect::<String>()));
        // `profile_id = excluded.profile_id` on conflict, so a re-pair always
        // ends at exactly what the code said. An ordinary unattributed code
        // therefore RELEASES a previous attribution rather than inheriting it:
        // `device_id` is the client's own self-reported id, so keeping the old
        // owner would let whoever re-pairs that id become the previous owner.
        // Losing an attribution is a narrowing and the operator can re-issue;
        // inheriting one is the impersonation this rung exists to prevent.
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
        .bind(&device_id)
        .bind(&device_name)
        .bind(&client_type)
        .bind(&attempt_at)
        .bind(&attempt_at)
        .bind(&attempt_at)
        .bind(&code_profile)
        .execute(&self.pool)
        .await;
        // The result was discarded outright before PAI-1's rung; it still does
        // not fail the pair (a client with a valid MAC has paired, whatever the
        // registry did), but it is no longer invisible. The attribution rides on
        // this statement, so a swallowed error here is a device that is silently
        // nobody's.
        if let Err(e) = &registered {
            tracing::warn!(
                error = %e,
                device = %device_id,
                "device row not written during pairing; the device is unattributed"
            );
        }

        let (session, refresh, expires) = self
            .issue_session_pair(&client_id, &client_type, &device_id)
            .await?;
        tracing::info!(
            client_id = %client_id,
            device = %device_name,
            expires_at = %expires.to_rfc3339(),
            "device paired (two-phase handshake)"
        );
        // The other half of the binding: a client that named a key gets back a
        // proof over the same transcript, which only something holding the
        // pairing code can produce. Without it, an interceptor that terminated
        // the client's TLS could invent an acceptance and a token of its own,
        // and the phone would be paired with the interceptor.
        let server_proof = request.channel_binding.as_deref().and_then(|spki| {
            pair_mac(
                code_plaintext.as_bytes(),
                SERVER_LABEL,
                &challenge,
                client_id.as_bytes(),
                Some(spki),
            )
            .map(|bytes| hex_lower(&bytes))
        });
        Ok(HandshakeResponse {
            server_proof,
            ..self.response(
                true,
                Some(session),
                Some(refresh),
                Some(expires.to_rfc3339()),
                None,
            )
        })
    }

    async fn refresh(&self, request: RefreshRequest) -> Result<HandshakeResponse> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let refresh_hash = Self::sha256_hex(request.refresh_token.as_bytes());
        let now = Utc::now().to_rfc3339();
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT client_id, client_type, device_id FROM session_tokens
             WHERE refresh_hash = ? AND revoked_at IS NULL AND refresh_expires_at > ?",
        )
        .bind(&refresh_hash)
        .bind(&now)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((client_id, client_type, device_id)) = row else {
            return Ok(self.reject("invalid_or_expired_refresh"));
        };

        // Rotate: revoke the old row, mint a fresh pair.
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE refresh_hash = ?")
            .bind(&now)
            .bind(&refresh_hash)
            .execute(&mut *tx)
            .await?;
        let (session, refresh, expires) = self
            .issue_session_pair_in(&mut tx, &client_id, &client_type, &device_id)
            .await?;
        tx.commit().await?;
        Ok(self.response(
            true,
            Some(session),
            Some(refresh),
            Some(expires.to_rfc3339()),
            None,
        ))
    }

    async fn issue_pairing_code_for(&self, profile_id: Option<&str>) -> Result<PairingCode> {
        // A blank id is refused rather than stored: it is neither NULL nor a
        // member, so `ON DELETE SET NULL` could never clear it and the delivery
        // query would return the device to nobody forever.
        let profile_id = profile_id.map(checked_profile_id).transpose()?;
        let code = Self::random_pairing_code();
        let code_hash = Self::sha256_hex(code.as_bytes());
        let now = Utc::now();
        let expires = now + Duration::minutes(PAIRING_CODE_TTL_MIN);

        // The foreign key on `profile_id` (migration 0043) does the existence
        // check: issuing a code for a member who has been deleted fails here
        // rather than minting a code that would attribute a phone to a ghost.
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
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        out.push(u8::from_str_radix(&s[i..i + 2], 16).ok()?);
    }
    Some(out)
}

/// Label for the MAC the client sends.
const CLIENT_LABEL: &[u8] = b"goose-pair-client-v1";
/// Label for the proof the server sends back.
const SERVER_LABEL: &[u8] = b"goose-pair-server-v1";

/// The MAC over a pairing transcript, keyed by the pairing code.
///
/// Two shapes, selected by whether the client had a channel to bind:
///
/// ```text
/// bound    label || 0x00 || challenge || 0x00 || client_id || 0x00 || spki
/// unbound  challenge || client_id
/// ```
///
/// The unbound shape is the original one and is kept byte-for-byte, because the
/// desktop dashboard and any phone running an older build still compute it.
///
/// `0x00` separates the bound fields unambiguously: `client_id` and the pin are
/// text and cannot contain a NUL, and the challenge is a fixed 32 bytes. The
/// label is inside the MAC rather than beside it so the two directions cannot
/// be replayed against each other -- a server proof is not a client MAC.
fn pair_mac(
    key: &[u8],
    label: &[u8],
    challenge: &[u8],
    client_id: &[u8],
    spki: Option<&str>,
) -> Option<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    match spki {
        Some(spki) => {
            mac.update(label);
            mac.update(&[0]);
            mac.update(challenge);
            mac.update(&[0]);
            mac.update(client_id);
            mac.update(&[0]);
            mac.update(spki.as_bytes());
        }
        None => {
            mac.update(challenge);
            mac.update(client_id);
        }
    }
    Some(mac.finalize().into_bytes().to_vec())
}

fn verify_mac(
    key: &[u8],
    challenge: &[u8],
    client_id: &[u8],
    spki: Option<&str>,
    expected: &[u8],
) -> bool {
    let Some(computed) = pair_mac(key, CLIENT_LABEL, challenge, client_id, spki) else {
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

    async fn fresh() -> SqliteHandshakeAdapter {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let adapter = SqliteHandshakeAdapter::new(db.system.clone(), None);
        // Keep the tempdir alive for the test (else the sqlite file vanishes
        // under the pool).
        std::mem::forget(tmp);
        adapter
    }

    /// As `fresh`, plus the pool and two household members, for the PAI-1
    /// attribution tests below.
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
        let adapter = SqliteHandshakeAdapter::new(db.system.clone(), None);
        std::mem::forget(tmp);
        (adapter, db.system)
    }

    /// Run the two-phase pair for `client_id` against `code`, honestly: the
    /// client only ever sees the plaintext code and the challenge, exactly as a
    /// phone does.
    async fn pair(hs: &SqliteHandshakeAdapter, code: &str, client_id: &str) -> HandshakeResponse {
        let init = hs
            .init_handshake(InitRequest {
                client_id: client_id.into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        hs.verify_handshake(VerifyRequest {
            challenge_id: init.challenge_id,
            mac: client_mac(code, &init.challenge, client_id),
            device_name: Some(format!("{client_id} phone")),
            channel_binding: None,
        })
        .await
        .unwrap()
    }

    /// The MAC a client computes when it bound the channel -- the same shape
    /// the app computes, spelled out here rather than reusing `pair_mac`, so a
    /// change to the transcript has to be made twice to go unnoticed.
    fn bound_client_mac(code: &str, challenge_b64: &str, client_id: &str, spki: &str) -> String {
        let challenge = B64.decode(challenge_b64.as_bytes()).unwrap();
        let mut mac = HmacSha256::new_from_slice(code.as_bytes()).unwrap();
        mac.update(b"goose-pair-client-v1");
        mac.update(&[0]);
        mac.update(&challenge);
        mac.update(&[0]);
        mac.update(client_id.as_bytes());
        mac.update(&[0]);
        mac.update(spki.as_bytes());
        hex_lower(&mac.finalize().into_bytes())
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
                channel_binding: None,
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "device-A"),
                device_name: Some("Test Phone".into()),
            })
            .await
            .unwrap();

        assert!(resp.accepted);
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

    /// Regression for DEF-4: verify_handshake used to hardcode device_type='gotg'
    /// (and pass "gotg" to issue_session_pair), discarding the real client_type
    /// sent at init. A desktop client must now persist device_type='desktop' and
    /// session_tokens.client_type='desktop'.
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
                channel_binding: None,
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
                channel_binding: None,
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
                channel_binding: None,
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
                channel_binding: None,
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
                channel_binding: None,
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
                channel_binding: None,
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

        // Force the stored expiry into the past, then re-check.
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

    /// Regression for the reported DoS: a LAN attacker spraying bad MACs must
    /// NOT lock out the operator's pairing code (no global failed-attempt
    /// counter anymore). The real device still pairs afterwards.
    #[tokio::test]
    async fn bad_mac_attempts_do_not_lock_out_pairing_code() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();

        for _ in 0..10 {
            let init = init_for(&hs, "attacker").await;
            let resp = hs
                .verify_handshake(VerifyRequest {
                    channel_binding: None,
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
                channel_binding: None,
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

    /// A challenge authorizes exactly one verify attempt: even a *failed*
    /// attempt burns it, so it can't be reused to keep guessing.
    #[tokio::test]
    async fn challenge_is_single_use_even_on_failure() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = init_for(&hs, "device-R").await;

        let bad = hs
            .verify_handshake(VerifyRequest {
                channel_binding: None,
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
                channel_binding: None,
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
                channel_binding: None,
                challenge_id: init.challenge_id,
                mac: "00".into(),
                device_name: None,
            })
            .await
            .unwrap();
        assert_eq!(resp.rejection_reason.as_deref(), Some("challenge_expired"));
    }

    /// After a refresh rotates the pair, the OLD refresh token is revoked and
    /// must not be replayable.
    #[tokio::test]
    async fn refresh_token_cannot_be_reused_after_rotation() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = init_for(&hs, "device-Q").await;
        let paired = hs
            .verify_handshake(VerifyRequest {
                channel_binding: None,
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

    // ── PAI-1: the device-to-profile rung ─────────────────────────────────
    //
    // These go through the real two-phase flow rather than writing
    // `devices.profile_id` by hand. `ProfileScope::Owner` was inert for a whole
    // phase because every fixture that produced an owned row set the column
    // directly and no production path did; a fixture production cannot produce
    // tests a system that does not exist.

    /// The rung, through the writers production uses: a code issued for Liz
    /// pairs a phone that is Liz's and is not Jerry's. The push-token hop off
    /// the same column is asserted in
    /// `sqlite_device_attribution::tests::a_profile_can_be_asked_for_its_devices_and_their_push_tokens`;
    /// no token is registered here, so this test does not claim it.
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

    /// The default, and what every shipped caller does today: an unattributed
    /// code pairs an unattributed device. It works, it is registered, and it is
    /// nobody's -- which is the truthful answer when nobody said who was
    /// pairing.
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

    /// The security property this design exists for. `PairedDevice` outranks
    /// every other rung of `identity_resolution::resolve`, so a device that
    /// could name its own member would outrank every proof the pond can make.
    /// The pairing client's body is deserialised with the extra field present
    /// and the device still belongs to whoever the CODE named.
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

        // Exactly the JSON a client would post, with an impersonation attempt
        // in it. If `VerifyRequest` ever grows a profile field and honours it,
        // this stops deserialising into "ignored" and the assertion below flips.
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

    /// Re-pairing a device id with an ordinary code releases the attribution
    /// rather than inheriting it. `device_id` is the client's own self-reported
    /// id, so inheriting would let whoever reuses it become the previous owner.
    /// Losing an attribution is a narrowing and the operator can re-issue.
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

    /// A code outstanding when its member is deleted degrades to an ordinary
    /// code. It must not fail the deletion and must not pair a phone to a ghost.
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

    // ---- Channel binding -------------------------------------------------
    //
    // The property these hold down: a phone that pinned the WRONG key ends
    // pairing in a visible failure instead of a successful pair with whoever
    // supplied that key. Without it, the pin has to reach the phone by a
    // trustworthy route, which in practice meant somebody reading fifty-one
    // characters of base64 off a dashboard and typing them without a slip.

    /// This pond's own pin, as `tls_identity::pin()` spells one.
    const OURS: &str = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";
    /// Somebody else's, for the interceptor to serve.
    const THEIRS: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    async fn fresh_pinned(pin: &str) -> SqliteHandshakeAdapter {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let adapter = SqliteHandshakeAdapter::new(db.system.clone(), Some(pin.to_string()));
        std::mem::forget(tmp);
        adapter
    }

    async fn challenge_for(hs: &SqliteHandshakeAdapter, client_id: &str) -> ChallengeResponse {
        hs.init_handshake(InitRequest {
            client_id: client_id.into(),
            client_type: "gotg".into(),
            client_version: "1.0".into(),
        })
        .await
        .unwrap()
    }

    /// The proof a client recomputes to satisfy itself it is talking to the
    /// pond that holds the code, and not to something that terminated its TLS.
    fn expected_server_proof(
        code: &str,
        challenge_b64: &str,
        client_id: &str,
        spki: &str,
    ) -> String {
        let challenge = B64.decode(challenge_b64.as_bytes()).unwrap();
        let mut mac = HmacSha256::new_from_slice(code.as_bytes()).unwrap();
        mac.update(b"goose-pair-server-v1");
        mac.update(&[0]);
        mac.update(&challenge);
        mac.update(&[0]);
        mac.update(client_id.as_bytes());
        mac.update(&[0]);
        mac.update(spki.as_bytes());
        hex_lower(&mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn a_bound_pair_succeeds_and_answers_with_a_proof_the_client_can_check() {
        let hs = fresh_pinned(OURS).await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hs, "device-bound").await;

        let mac = bound_client_mac(&pc.code, &init.challenge, "device-bound", OURS);
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: mac.clone(),
                device_name: None,
                channel_binding: Some(OURS.into()),
            })
            .await
            .unwrap();

        assert!(resp.accepted, "{:?}", resp.rejection_reason);
        assert_eq!(
            resp.server_proof.as_deref(),
            Some(expected_server_proof(&pc.code, &init.challenge, "device-bound", OURS).as_str()),
        );
        assert_ne!(
            resp.server_proof.as_deref(),
            Some(mac.as_str()),
            "the two directions must not share a transcript, or the proof is just the \
             client's own MAC handed back and proves nothing"
        );
    }

    /// The case the binding exists for: something on the LAN answers as the
    /// pond -- an mDNS reply is enough -- and offers its own key. The phone
    /// pins it, and MACs over it.
    #[tokio::test]
    async fn a_client_that_pinned_an_interceptors_key_cannot_pair() {
        let hs = fresh_pinned(OURS).await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hs, "device-mitm").await;

        let relayed = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id.clone(),
                mac: bound_client_mac(&pc.code, &init.challenge, "device-mitm", THEIRS),
                device_name: None,
                channel_binding: Some(THEIRS.into()),
            })
            .await
            .unwrap();
        assert!(!relayed.accepted);
        assert_eq!(
            relayed.rejection_reason.as_deref(),
            Some("channel_binding_mismatch")
        );

        // And the refusal costs the honest client nothing: the challenge is
        // checked before it is consumed, so the same one still works once the
        // phone is pointed at the real pond.
        let honest = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: bound_client_mac(&pc.code, &init.challenge, "device-mitm", OURS),
                device_name: None,
                channel_binding: Some(OURS.into()),
            })
            .await
            .unwrap();
        assert!(honest.accepted, "{:?}", honest.rejection_reason);
    }

    /// The downgrade. An interceptor cannot pass the bound MAC, so the next
    /// thing it would try is to drop the field and let the unbound branch take
    /// the MAC instead. That branch computes a different transcript, and
    /// computing either one needs the pairing code the interceptor does not
    /// have.
    #[tokio::test]
    async fn stripping_the_binding_off_a_bound_mac_does_not_downgrade_it() {
        let hs = fresh_pinned(OURS).await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hs, "device-strip").await;

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: bound_client_mac(&pc.code, &init.challenge, "device-strip", THEIRS),
                device_name: None,
                channel_binding: None,
            })
            .await
            .unwrap();
        assert!(!resp.accepted);
        assert_eq!(resp.rejection_reason.as_deref(), Some("invalid_mac"));
    }

    /// A pond with no TLS identity has nothing to compare a claimed binding
    /// against. Refusing is the narrowing answer; ignoring the claim would
    /// make the field advisory, and an advisory binding is one an interceptor
    /// can strip.
    #[tokio::test]
    async fn a_binding_claimed_against_a_pond_with_no_identity_is_refused() {
        let hs = fresh().await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hs, "device-noid").await;

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: bound_client_mac(&pc.code, &init.challenge, "device-noid", OURS),
                device_name: None,
                channel_binding: Some(OURS.into()),
            })
            .await
            .unwrap();
        assert!(!resp.accepted);
        assert_eq!(
            resp.rejection_reason.as_deref(),
            Some("channel_binding_mismatch")
        );
    }

    /// Removing a device has to remove its access.
    ///
    /// `session_tokens.device_id` carries no foreign key and nothing cascades
    /// onto that table, so for as long as this did not exist, deleting a
    /// `devices` row left every token it had been issued validating happily.
    /// An operator removing a lost phone was told it was gone.
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
        // And the refresh token dies with it, or the phone simply mints a new
        // session and the revocation lasts until the next expiry.
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
        // Idempotent, because the delete path calls it before dropping the row
        // and a retried delete must not fail on the second attempt.
        assert_eq!(hs.revoke_device("never-paired").await.unwrap(), 0);
        let code = hs.issue_pairing_code().await.unwrap().code;
        assert!(pair(&hs, &code, "phone").await.accepted);
        assert_eq!(hs.revoke_device("phone").await.unwrap(), 1);
        assert_eq!(hs.revoke_device("phone").await.unwrap(), 0);
    }

    /// The transcript, against vectors computed outside both implementations.
    ///
    /// These exact strings are asserted again in the app, in
    /// `goose-on-the-go/services/__tests__/hmac.test.ts :: the pairing
    /// transcript`. That is the point of them: the two sides compute the same
    /// bytes in different languages, and a change to either that nobody carried
    /// across shows up here as a failing vector rather than in the field as a
    /// pairing that will not complete and says only `invalid_mac`.
    #[test]
    fn the_transcript_matches_the_app() {
        let challenge: Vec<u8> = (0u8..32).collect();
        let code = b"482915";
        let client = b"install-abcdef";
        let spki = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";

        let mac = |label: &[u8], spki: Option<&str>| {
            hex_lower(&pair_mac(code, label, &challenge, client, spki).unwrap())
        };
        assert_eq!(
            mac(CLIENT_LABEL, Some(spki)),
            "2cc63017ff6979d0f1c4009a4f37c3c95bc2d535897fc2fb19a0a496f6a649aa",
        );
        assert_eq!(
            mac(SERVER_LABEL, Some(spki)),
            "73171bac55bb3346045142bb0647057067d94712f5fab82d2260bf91c106ebe0",
        );
        // The original shape, for a client older than the binding. It has to
        // stay byte-for-byte what it was or every such client stops pairing.
        assert_eq!(
            mac(CLIENT_LABEL, None),
            "b922abd25f5f1519c9d0f8ccabab4ed17b3e888a3b868be9cfdc320de017ec30",
        );
    }

    /// The dashboard pairs over loopback HTTP, where there is no certificate to
    /// bind to and nothing in the middle to bind against. It keeps the original
    /// transcript, and gets no proof back because there is none to make.
    #[tokio::test]
    async fn an_unbound_client_still_pairs_with_a_pinned_pond() {
        let hs = fresh_pinned(OURS).await;
        let pc = hs.issue_pairing_code().await.unwrap();
        let init = challenge_for(&hs, "dashboard").await;

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: client_mac(&pc.code, &init.challenge, "dashboard"),
                device_name: None,
                channel_binding: None,
            })
            .await
            .unwrap();
        assert!(resp.accepted, "{:?}", resp.rejection_reason);
        assert_eq!(resp.server_proof, None);
    }
}
