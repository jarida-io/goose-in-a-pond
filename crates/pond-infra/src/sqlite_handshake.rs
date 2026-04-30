//! SQLite-backed [`Handshake`] adapter implementing the two-phase pairing
//! protocol described in `pond-core::ports::handshake`.
//!
//! Layout (see `migrations/system/0015_handshake.sql`):
//! - `pairing_codes`        — single-use codes (sha256-hashed)
//! - `handshake_challenges` — short-lived challenges per init
//! - `session_tokens`       — issued session+refresh tokens (sha256-hashed)
//!
//! All token material returned to clients is opaque base64 of 32 random
//! bytes; only sha256 hashes are persisted. MAC verification is constant-time.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::{Pool, Sqlite};
use std::sync::Arc;
use subtle::ConstantTimeEq;

use pond_core::ports::handshake::{
    ChallengeResponse, Handshake, HandshakeRequest, HandshakeResponse, InitRequest, PairingCode,
    RefreshRequest, VerifyRequest,
};

const PAIRING_CODE_TTL_MIN: i64 = 10;
const CHALLENGE_TTL_SEC: i64 = 60;
const SESSION_TTL_HOURS: i64 = 24;
const REFRESH_TTL_DAYS: i64 = 30;
const PAIRING_MAX_FAILED_ATTEMPTS: i64 = 5;

/// SQLite-backed implementation of the `Handshake` port.
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

    /// Wrap with `Arc<dyn Handshake>`.
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
        // 6-digit numeric, leading zeros preserved.
        let mut buf = [0u8; 4];
        OsRng.fill_bytes(&mut buf);
        let n = u32::from_le_bytes(buf) % 1_000_000;
        format!("{:06}", n)
    }

    fn build_response(
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

    /// Mint a new session+refresh token pair, persist hashed, and return the
    /// plaintext to the caller.
    ///
    /// Side effect: revokes any previously-active session+refresh pairs for
    /// the same `device_id`. A device that re-pairs (e.g. the user runs the
    /// wizard a second time on the same phone) ends up with exactly one
    /// active row, not a growing pile of orphans.
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

        // Revoke any prior active sessions for this device — a re-pair from
        // the same install should supersede, not accumulate.
        sqlx::query(
            "UPDATE session_tokens SET revoked_at = ?
             WHERE device_id = ? AND revoked_at IS NULL",
        )
        .bind(now.to_rfc3339())
        .bind(device_id)
        .execute(&self.pool)
        .await?;

        let token_hash = Self::sha256_hex(session_token.as_bytes());
        let refresh_hash = Self::sha256_hex(refresh_token.as_bytes());

        sqlx::query(
            "INSERT INTO session_tokens (token_hash, refresh_hash, device_id, client_id,
                client_type, created_at, expires_at, refresh_expires_at, last_seen_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&token_hash)
        .bind(&refresh_hash)
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

    /// Look up a pairing code by trying it against all active rows.
    /// Returns the matched row's `code_hash` so callers can mark it consumed.
    async fn match_pairing_code(&self, code: &str) -> Result<Option<String>> {
        let now = Self::now().to_rfc3339();
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT code_hash FROM pairing_codes
             WHERE consumed_at IS NULL
               AND expires_at > ?
               AND failed_attempts < ?
             ORDER BY created_at DESC",
        )
        .bind(&now)
        .bind(PAIRING_MAX_FAILED_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;

        let candidate_hash = Self::sha256_hex(code.as_bytes());
        for (stored,) in rows {
            // Constant-time compare on the hash (both fixed-length hex).
            if stored.as_bytes().ct_eq(candidate_hash.as_bytes()).into() {
                return Ok(Some(stored));
            }
        }
        Ok(None)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[async_trait]
impl Handshake for SqliteHandshakeAdapter {
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse> {
        // Legacy path — used when a client already knows a pairing code
        // and skips the challenge step. Verify the code, then mint a token.
        let Some(code) = request.pairing_code.as_deref() else {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("missing_pairing_code".to_string()),
            ));
        };
        let Some(matched) = self.match_pairing_code(code).await? else {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("invalid_or_expired_pairing_code".to_string()),
            ));
        };

        // Consume the code (single-use).
        sqlx::query("UPDATE pairing_codes SET consumed_at = ? WHERE code_hash = ?")
            .bind(Self::now().to_rfc3339())
            .bind(&matched)
            .execute(&self.pool)
            .await?;

        let device_id = request.client_id.clone();
        let (session, refresh, expires) = self
            .issue_session_pair(&request.client_id, &request.client_type, &device_id)
            .await?;

        Ok(self.build_response(
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
             WHERE token_hash = ?
               AND revoked_at IS NULL
               AND expires_at > ?",
        )
        .bind(&token_hash)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await?;

        if row.is_some() {
            // Best-effort touch of last_seen_at for device-tracking.
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
        let token_hash = Self::sha256_hex(token.as_bytes());
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE token_hash = ?")
            .bind(Self::now().to_rfc3339())
            .bind(&token_hash)
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
        // 1. Look up the challenge.
        let row: Option<(String, Vec<u8>, String, Option<String>)> = sqlx::query_as(
            "SELECT client_id, challenge, expires_at, consumed_at
             FROM handshake_challenges WHERE id = ?",
        )
        .bind(&request.challenge_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((client_id, challenge, expires_at, consumed_at)) = row else {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("unknown_challenge".to_string()),
            ));
        };
        if consumed_at.is_some() {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("challenge_used".to_string()),
            ));
        }
        let exp = DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|e| anyhow!("bad expires_at: {e}"))?;
        if exp < Utc::now() {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("challenge_expired".to_string()),
            ));
        }

        // 2. For each active pairing code, compute the expected MAC and compare.
        let now = Utc::now().to_rfc3339();
        let codes: Vec<(String,)> = sqlx::query_as(
            "SELECT code_hash FROM pairing_codes
             WHERE consumed_at IS NULL AND expires_at > ?
               AND failed_attempts < ?",
        )
        .bind(&now)
        .bind(PAIRING_MAX_FAILED_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;

        if codes.is_empty() {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("no_active_pairing_code".to_string()),
            ));
        }

        // The verifier doesn't know the code; the client used it as the HMAC
        // key. We therefore can't recompute the MAC server-side without the
        // plaintext code. Two designs are possible:
        //   (a) Store plaintext codes (risky).
        //   (b) Have the client also return a hint or the code itself.
        //
        // For correctness + secrecy we instead require the client to send
        // `mac` AS the code-derived proof, but verify it as
        //   HMAC(code_plaintext, challenge || client_id) == mac
        // For that we need the plaintext code in this scope. The chosen
        // tradeoff: keep the code in memory **only between issuance and
        // first verification** by also storing it (encrypted with the host
        // hostname) — but for the v1 implementation we accept storing the
        // plaintext code in `pairing_codes.code_plaintext` until it is
        // consumed, then NULL it. See `consume_code_plaintext`.
        //
        // The migration above only stores the hash; v1 ships with an
        // additive column added at runtime if missing. To avoid a second
        // migration, we use a single-row in-memory cache keyed by code_hash
        // for codes issued during this process — see `IssuedCodeCache`
        // below. Codes issued from a previous process are unusable, which
        // matches the operator workflow (read the code displayed at THIS
        // server startup).
        let mac_bytes = hex_decode(&request.mac).ok_or_else(|| anyhow!("bad mac hex"))?;

        let cache = ISSUED_CODE_CACHE.read().await;
        let mut matched_hash: Option<String> = None;
        for (code_hash,) in codes {
            if let Some(plaintext) = cache.get(&code_hash) {
                if verify_mac(plaintext.as_bytes(), &challenge, client_id.as_bytes(), &mac_bytes) {
                    matched_hash = Some(code_hash);
                    break;
                }
            }
        }
        drop(cache);

        let Some(code_hash) = matched_hash else {
            // Increment failed-attempt counters on every active code (we can't
            // pinpoint which one was attempted — best-effort lockout).
            let _ = sqlx::query(
                "UPDATE pairing_codes SET failed_attempts = failed_attempts + 1
                 WHERE consumed_at IS NULL",
            )
            .execute(&self.pool)
            .await;
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("invalid_mac".to_string()),
            ));
        };

        // 3. Consume challenge + pairing code.
        let consumed_at = Utc::now().to_rfc3339();
        sqlx::query("UPDATE handshake_challenges SET consumed_at = ? WHERE id = ?")
            .bind(&consumed_at)
            .bind(&request.challenge_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE pairing_codes SET consumed_at = ? WHERE code_hash = ?")
            .bind(&consumed_at)
            .bind(&code_hash)
            .execute(&self.pool)
            .await?;
        ISSUED_CODE_CACHE.write().await.remove(&code_hash);

        // 4. Upsert into devices table and mint tokens.
        let device_id = client_id.clone();
        let device_name = request
            .device_name
            .clone()
            .unwrap_or_else(|| format!("gotg-{}", &client_id[..client_id.len().min(8)]));

        // Best-effort — schema for `devices` is in 0001/0004; if the row
        // already exists we just refresh `last_seen` / name.
        let _ = sqlx::query(
            "INSERT INTO devices (id, name, hostname, device_type, ip_address,
                capabilities, last_seen, is_online, created_at, updated_at)
             VALUES (?, ?, '', 'gotg', '', '[]', ?, 1, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               last_seen = excluded.last_seen,
               is_online = 1,
               updated_at = excluded.updated_at",
        )
        .bind(&device_id)
        .bind(&device_name)
        .bind(&consumed_at)
        .bind(&consumed_at)
        .bind(&consumed_at)
        .execute(&self.pool)
        .await;

        let (session, refresh, expires) = self
            .issue_session_pair(&client_id, "gotg", &device_id)
            .await?;

        Ok(self.build_response(
            true,
            Some(session),
            Some(refresh),
            Some(expires.to_rfc3339()),
            None,
        ))
    }

    async fn refresh(&self, request: RefreshRequest) -> Result<HandshakeResponse> {
        let refresh_hash = Self::sha256_hex(request.refresh_token.as_bytes());
        let now = Utc::now();
        let now_rfc = now.to_rfc3339();

        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT client_id, client_type, device_id
             FROM session_tokens
             WHERE refresh_hash = ?
               AND revoked_at IS NULL
               AND refresh_expires_at > ?",
        )
        .bind(&refresh_hash)
        .bind(&now_rfc)
        .fetch_optional(&self.pool)
        .await?;

        let Some((client_id, client_type, device_id)) = row else {
            return Ok(self.build_response(
                false,
                None,
                None,
                None,
                Some("invalid_or_expired_refresh".to_string()),
            ));
        };

        // Rotate: revoke the old row, issue a fresh pair.
        sqlx::query("UPDATE session_tokens SET revoked_at = ? WHERE refresh_hash = ?")
            .bind(&now_rfc)
            .bind(&refresh_hash)
            .execute(&self.pool)
            .await?;

        let (session, refresh, expires) = self
            .issue_session_pair(&client_id, &client_type, &device_id)
            .await?;

        Ok(self.build_response(
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
            "INSERT INTO pairing_codes (code_hash, created_at, expires_at)
             VALUES (?, ?, ?)",
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

        Ok(PairingCode {
            code,
            expires_at: expires.to_rfc3339(),
        })
    }

    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        let now = Utc::now().to_rfc3339();
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT code_hash, expires_at FROM pairing_codes
             WHERE consumed_at IS NULL AND expires_at > ?
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&now)
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

// Plaintext pairing codes are kept process-local: the operator reads them
// off this server's stdout/dashboard, and clients must verify before the
// process restarts. On restart, any unused codes from a prior run are
// silently invalidated (no plaintext available to verify against).
use once_cell::sync::Lazy;
use std::collections::HashMap;
use tokio::sync::RwLock;
static ISSUED_CODE_CACHE: Lazy<RwLock<HashMap<String, String>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

fn verify_mac(key: &[u8], challenge: &[u8], client_id: &[u8], expected: &[u8]) -> bool {
    type HmacSha256 = Hmac<Sha256>;
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return false;
    };
    mac.update(challenge);
    mac.update(client_id);
    mac.verify_slice(expected).is_ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn fresh() -> SqliteHandshakeAdapter {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let adapter = SqliteHandshakeAdapter::new(db.system.clone());
        // Leak the tempdir for the duration of the test (otherwise the
        // sqlite file is removed under the pool's feet).
        std::mem::forget(tmp);
        adapter
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

        let challenge = B64.decode(init.challenge.as_bytes()).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(pc.code.as_bytes()).unwrap();
        mac.update(&challenge);
        mac.update(b"device-A");
        let mac_hex = hex_lower(&mac.finalize().into_bytes());

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: mac_hex,
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
    async fn wrong_code_rejected() {
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
        let challenge = B64.decode(init.challenge.as_bytes()).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"000000").unwrap();
        mac.update(&challenge);
        mac.update(b"device-B");
        let mac_hex = hex_lower(&mac.finalize().into_bytes());

        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: mac_hex,
                device_name: None,
            })
            .await
            .unwrap();
        assert!(!resp.accepted);
        assert_eq!(resp.rejection_reason.as_deref(), Some("invalid_mac"));
    }

    #[tokio::test]
    async fn re_pair_same_device_revokes_prior_sessions() {
        let hs = fresh().await;

        // First pairing.
        let pc1 = hs.issue_pairing_code().await.unwrap();
        let init1 = hs
            .init_handshake(InitRequest {
                client_id: "device-D".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let challenge1 = B64.decode(init1.challenge.as_bytes()).unwrap();
        let mut mac1 = Hmac::<Sha256>::new_from_slice(pc1.code.as_bytes()).unwrap();
        mac1.update(&challenge1);
        mac1.update(b"device-D");
        let resp1 = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init1.challenge_id,
                mac: hex_lower(&mac1.finalize().into_bytes()),
                device_name: Some("Phone".into()),
            })
            .await
            .unwrap();
        let token1 = resp1.session_token.unwrap();
        assert!(hs.validate_token(&token1).await.unwrap());

        // Second pairing from the same device — same client_id.
        let pc2 = hs.issue_pairing_code().await.unwrap();
        let init2 = hs
            .init_handshake(InitRequest {
                client_id: "device-D".into(),
                client_type: "gotg".into(),
                client_version: "1.0".into(),
            })
            .await
            .unwrap();
        let challenge2 = B64.decode(init2.challenge.as_bytes()).unwrap();
        let mut mac2 = Hmac::<Sha256>::new_from_slice(pc2.code.as_bytes()).unwrap();
        mac2.update(&challenge2);
        mac2.update(b"device-D");
        let resp2 = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init2.challenge_id,
                mac: hex_lower(&mac2.finalize().into_bytes()),
                device_name: Some("Phone".into()),
            })
            .await
            .unwrap();
        let token2 = resp2.session_token.unwrap();

        // Old token is revoked, new one is live.
        assert!(!hs.validate_token(&token1).await.unwrap(), "old session must be revoked");
        assert!(hs.validate_token(&token2).await.unwrap(), "new session must be live");
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
        let challenge = B64.decode(init.challenge.as_bytes()).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(pc.code.as_bytes()).unwrap();
        mac.update(&challenge);
        mac.update(b"device-C");
        let resp = hs
            .verify_handshake(VerifyRequest {
                challenge_id: init.challenge_id,
                mac: hex_lower(&mac.finalize().into_bytes()),
                device_name: None,
            })
            .await
            .unwrap();
        assert!(resp.accepted);
        let old_session = resp.session_token.clone().unwrap();
        let refresh = resp.refresh_token.unwrap();

        let resp2 = hs.refresh(RefreshRequest { refresh_token: refresh }).await.unwrap();
        assert!(resp2.accepted);
        let new_session = resp2.session_token.unwrap();
        assert_ne!(old_session, new_session);
        assert!(!hs.validate_token(&old_session).await.unwrap()); // old revoked
        assert!(hs.validate_token(&new_session).await.unwrap());
    }
}
