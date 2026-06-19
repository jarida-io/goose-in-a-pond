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
    RefreshRequest, VerifyRequest,
};

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
}

impl SqliteHandshakeAdapter {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "localhost".to_string());
        Self {
            pool,
            hostname,
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
        .execute(&self.pool)
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
        .execute(&self.pool)
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

    async fn revoke_token(&self, token: &str) -> Result<()> {
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE token_hash = ?")
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
            "INSERT INTO handshake_challenges (id, client_id, challenge, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&request.client_id)
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
        let row: Option<(String, Vec<u8>, String, Option<String>)> = sqlx::query_as(
            "SELECT client_id, challenge, expires_at, consumed_at
             FROM handshake_challenges WHERE id = ?",
        )
        .bind(&request.challenge_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((client_id, challenge, expires_at, consumed_at)) = row else {
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

        let mut matched_hash: Option<String> = None;
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
                        matched_hash = Some(code_hash.clone());
                        break;
                    }
                }
            }
        }

        let Some(code_hash) = matched_hash else {
            // Wrong code/MAC. The challenge is already spent (step 2) and the
            // pairing code is untouched, so a bad guess can neither be retried
            // on this challenge nor lock the operator out.
            return Ok(self.reject("invalid_mac"));
        };

        // 4. Consume the matched code (single-use on success), register the
        //    device, then mint tokens.
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
        let _ = sqlx::query(
            "INSERT INTO devices (id, name, hostname, device_type, ip_address,
                capabilities, last_seen, is_online, created_at, updated_at)
             VALUES (?, ?, '', 'gotg', '', '[]', ?, 1, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, last_seen = excluded.last_seen,
               is_online = 1, updated_at = excluded.updated_at",
        )
        .bind(&device_id)
        .bind(&device_name)
        .bind(&attempt_at)
        .bind(&attempt_at)
        .bind(&attempt_at)
        .execute(&self.pool)
        .await;

        let (session, refresh, expires) = self
            .issue_session_pair(&client_id, "gotg", &device_id)
            .await?;
        tracing::info!(
            client_id = %client_id,
            device = %device_name,
            expires_at = %expires.to_rfc3339(),
            "device paired (two-phase handshake)"
        );
        Ok(self.response(
            true,
            Some(session),
            Some(refresh),
            Some(expires.to_rfc3339()),
            None,
        ))
    }

    async fn refresh(&self, request: RefreshRequest) -> Result<HandshakeResponse> {
        let refresh_hash = Self::sha256_hex(request.refresh_token.as_bytes());
        let now = Utc::now().to_rfc3339();
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT client_id, client_type, device_id FROM session_tokens
             WHERE refresh_hash = ? AND revoked_at IS NULL AND refresh_expires_at > ?",
        )
        .bind(&refresh_hash)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await?;

        let Some((client_id, client_type, device_id)) = row else {
            return Ok(self.reject("invalid_or_expired_refresh"));
        };

        // Rotate: revoke the old row, mint a fresh pair.
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE refresh_hash = ?")
            .bind(&now)
            .bind(&refresh_hash)
            .execute(&self.pool)
            .await?;
        let (session, refresh, expires) = self
            .issue_session_pair(&client_id, &client_type, &device_id)
            .await?;
        Ok(self.response(
            true,
            Some(session),
            Some(refresh),
            Some(expires.to_rfc3339()),
            None,
        ))
    }

    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        let code = Self::random_pairing_code();
        let code_hash = Self::sha256_hex(code.as_bytes());
        let now = Utc::now();
        let expires = now + Duration::minutes(PAIRING_CODE_TTL_MIN);

        sqlx::query(
            "INSERT INTO pairing_codes (code_hash, created_at, expires_at) VALUES (?, ?, ?)",
        )
        .bind(&code_hash)
        .bind(now.to_rfc3339())
        .bind(expires.to_rfc3339())
        .execute(&self.pool)
        .await?;
        ISSUED_CODE_CACHE
            .write()
            .await
            .insert(code_hash, code.clone());
        tracing::info!(
            expires_at = %expires.to_rfc3339(),
            "issued pairing code (valid {PAIRING_CODE_TTL_MIN}m)"
        );

        Ok(PairingCode {
            code,
            expires_at: expires.to_rfc3339(),
        })
    }

    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT code_hash, expires_at FROM pairing_codes
             WHERE consumed_at IS NULL AND expires_at > ?
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?;
        let Some((hash, expires_at)) = row else {
            return Ok(None);
        };
        let cache = ISSUED_CODE_CACHE.read().await;
        Ok(cache.get(&hash).map(|code| PairingCode {
            code: code.clone(),
            expires_at,
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

fn verify_mac(key: &[u8], challenge: &[u8], client_id: &[u8], expected: &[u8]) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return false;
    };
    mac.update(challenge);
    mac.update(client_id);
    mac.verify_slice(expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn fresh() -> SqliteHandshakeAdapter {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let adapter = SqliteHandshakeAdapter::new(db.system.clone());
        // Keep the tempdir alive for the test (else the sqlite file vanishes
        // under the pool).
        std::mem::forget(tmp);
        adapter
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
        let token = resp.session_token.unwrap();
        assert!(hs.validate_token(&token).await.unwrap());
        hs.revoke_token(&token).await.unwrap();
        assert!(!hs.validate_token(&token).await.unwrap());
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

    /// A challenge authorizes exactly one verify attempt: even a *failed*
    /// attempt burns it, so it can't be reused to keep guessing.
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

    /// After a refresh rotates the pair, the OLD refresh token is revoked and
    /// must not be replayable.
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
}
