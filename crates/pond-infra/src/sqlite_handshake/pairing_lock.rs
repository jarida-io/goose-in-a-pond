//! The pairing lock's rows: the main member, the lock, the recovery PIN, and the requests of
//! phones waiting to be let in. A child of the pairing adapter, since letting a phone in is the
//! adapter's own registering of a device and minting of its session.

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use pond_core::mcp::ports::notification_recipients::PHONE_CLIENT_TYPE;
use pond_core::security::domain::pairing_lock::{
    lockout_after, pin_is_acceptable, PairedTokens, PairingClaim, PairingLockState, PairingRequest,
    PendingPairing, PinCheck, FREE_PIN_TRIES, MAX_PENDING_REQUESTS, REQUEST_TTL_MINUTES,
};
use sha2::Sha256;
use sqlx::Row;
use subtle::ConstantTimeEq;

use super::SqliteHandshakeAdapter;

const SCHEME: &str = "pbkdf2-sha256";
/// A PIN is short, so the hash is not what protects it: the count of wrong tries is. Enough
/// rounds that a check is felt, few enough for the Orin to answer at once.
const PIN_ROUNDS: u32 = 200_000;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD_NO_PAD
}

fn derive(pin: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(pin.as_bytes(), salt, rounds, &mut out);
    out
}

fn verifier(pin: &str) -> String {
    let salt: [u8; 16] = rand::random();
    format!(
        "{SCHEME}${PIN_ROUNDS}${}${}",
        b64().encode(salt),
        b64().encode(derive(pin, &salt, PIN_ROUNDS))
    )
}

fn matches(verifier: &str, pin: &str) -> Result<bool> {
    let mut parts = verifier.split('$');
    let (Some(SCHEME), Some(rounds), Some(salt), Some(hash), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        bail!("the recovery PIN's verifier is not one this build reads");
    };
    let rounds: u32 = rounds.parse().context("the recovery PIN's rounds")?;
    let salt = b64().decode(salt).context("the recovery PIN's salt")?;
    let expected = b64().decode(hash).context("the recovery PIN's hash")?;
    Ok(derive(pin, &salt, rounds.max(1))
        .ct_eq(expected.as_slice())
        .into())
}

/// What `complete_pairing` needs to open a request in place of a pairing.
pub(super) struct Asking<'a> {
    pub client_id: &'a str,
    pub client_type: &'a str,
    pub device_name: &'a str,
    pub profile_id: Option<&'a str>,
}

impl SqliteHandshakeAdapter {
    pub(super) async fn lock_state(&self) -> Result<PairingLockState> {
        let row = sqlx::query(
            "SELECT owner_profile_id, locked, pin_verifier FROM pairing_lock WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(PairingLockState {
                locked: true,
                owner_profile_id: None,
                has_recovery_pin: false,
                owner_has_a_phone: false,
            });
        };
        let owner_profile_id: Option<String> = row.get("owner_profile_id");
        let owner_has_a_phone = match &owner_profile_id {
            Some(owner) => sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM devices d WHERE d.profile_id = ? AND d.device_type = ? \
                 AND EXISTS (SELECT 1 FROM session_tokens t \
                             WHERE t.device_id = d.id AND t.revoked_at IS NULL) LIMIT 1",
            )
            .bind(owner)
            .bind(PHONE_CLIENT_TYPE)
            .fetch_optional(&self.pool)
            .await?
            .is_some(),
            None => false,
        };
        Ok(PairingLockState {
            locked: row.get::<i64, _>("locked") != 0,
            owner_profile_id,
            has_recovery_pin: row.get::<Option<String>, _>("pin_verifier").is_some(),
            owner_has_a_phone,
        })
    }

    pub(super) async fn lock_set(&self, locked: bool) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO pairing_lock (id) VALUES (1)")
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE pairing_lock SET locked = ? WHERE id = 1")
            .bind(i64::from(locked))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(super) async fn lock_owner(&self, profile_id: &str, replace: bool) -> Result<bool> {
        sqlx::query("INSERT OR IGNORE INTO pairing_lock (id) VALUES (1)")
            .execute(&self.pool)
            .await?;
        let sql = if replace {
            "UPDATE pairing_lock SET owner_profile_id = ? WHERE id = 1"
        } else {
            "UPDATE pairing_lock SET owner_profile_id = ? WHERE id = 1 AND owner_profile_id IS NULL"
        };
        let changed = sqlx::query(sql)
            .bind(profile_id)
            .execute(&self.pool)
            .await
            .context("naming the pond's main member")?;
        Ok(changed.rows_affected() > 0)
    }

    pub(super) async fn lock_pin(&self, pin: Option<&str>) -> Result<()> {
        let stored = match pin {
            Some(pin) if pin_is_acceptable(pin) => Some(verifier(pin)),
            Some(_) => bail!("a recovery PIN is six to twelve digits"),
            None => None,
        };
        sqlx::query("INSERT OR IGNORE INTO pairing_lock (id) VALUES (1)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "UPDATE pairing_lock SET pin_verifier = ?, pin_failures = 0, pin_locked_until = NULL \
             WHERE id = 1",
        )
        .bind(stored)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Checks a PIN and keeps the count of wrong ones with it, so a restart forgives nothing.
    async fn check_pin(&self, pin: &str, now: DateTime<Utc>) -> Result<PinCheck> {
        let row = sqlx::query(
            "SELECT pin_verifier, pin_failures, pin_locked_until FROM pairing_lock WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(PinCheck::NoPin);
        };
        let Some(stored) = row.get::<Option<String>, _>("pin_verifier") else {
            return Ok(PinCheck::NoPin);
        };
        let failures = row.get::<i64, _>("pin_failures").max(0) as u32;
        let locked_until = row
            .get::<Option<String>, _>("pin_locked_until")
            .and_then(|at| DateTime::parse_from_rfc3339(&at).ok())
            .map(|at| at.with_timezone(&Utc));
        if let Some(until) = locked_until.filter(|until| *until > now) {
            return Ok(PinCheck::LockedFor(until - now));
        }
        if matches(&stored, pin)? {
            sqlx::query(
                "UPDATE pairing_lock SET pin_failures = 0, pin_locked_until = NULL WHERE id = 1",
            )
            .execute(&self.pool)
            .await?;
            return Ok(PinCheck::Accepted);
        }
        let failures = failures + 1;
        let wait = lockout_after(failures);
        sqlx::query("UPDATE pairing_lock SET pin_failures = ?, pin_locked_until = ? WHERE id = 1")
            .bind(i64::from(failures))
            .bind(wait.map(|wait| (now + wait).to_rfc3339()))
            .execute(&self.pool)
            .await?;
        Ok(match wait {
            Some(wait) => PinCheck::LockedFor(wait),
            None => PinCheck::Wrong {
                tries_left: FREE_PIN_TRIES - failures,
            },
        })
    }

    /// Opens a request for a phone that proved its code. One per phone: a second try replaces
    /// the first, so a phone that asks twice is not two rows for somebody to answer.
    pub(super) async fn open_request(&self, asking: Asking<'_>) -> Result<Option<PendingPairing>> {
        let now = Self::now();
        sqlx::query(
            "DELETE FROM pairing_requests \
             WHERE (status = 'pending' AND client_id = ?) OR expires_at <= ?",
        )
        .bind(asking.client_id)
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        let waiting: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pairing_requests WHERE status = 'pending'")
                .fetch_one(&self.pool)
                .await?;
        if waiting as usize >= MAX_PENDING_REQUESTS {
            return Ok(None);
        }
        let pending = PendingPairing {
            request_id: uuid::Uuid::new_v4().to_string(),
            claim_secret: Self::random_token(),
            expires_at: (now + Duration::minutes(REQUEST_TTL_MINUTES)).to_rfc3339(),
        };
        sqlx::query(
            "INSERT INTO pairing_requests \
             (id, client_id, client_type, device_name, profile_id, claim_hash, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&pending.request_id)
        .bind(asking.client_id)
        .bind(asking.client_type)
        .bind(asking.device_name)
        .bind(asking.profile_id)
        .bind(Self::sha256_hex(pending.claim_secret.as_bytes()))
        .bind(now.to_rfc3339())
        .bind(&pending.expires_at)
        .execute(&self.pool)
        .await
        .context("opening a pairing request")?;
        Ok(Some(pending))
    }

    pub(super) async fn waiting_requests(&self) -> Result<Vec<PairingRequest>> {
        let rows = sqlx::query(
            "SELECT id, client_id, client_type, device_name, profile_id, created_at, expires_at \
             FROM pairing_requests WHERE status = 'pending' AND expires_at > ? \
             ORDER BY created_at, id",
        )
        .bind(Self::now().to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| PairingRequest {
                id: row.get("id"),
                client_id: row.get("client_id"),
                client_type: row.get("client_type"),
                device_name: row.get("device_name"),
                profile_id: row.get("profile_id"),
                created_at: row.get("created_at"),
                expires_at: row.get("expires_at"),
            })
            .collect())
    }

    pub(super) async fn decide_request(
        &self,
        request_id: &str,
        accept: bool,
        by_device: Option<&str>,
        profile_id: Option<&str>,
    ) -> Result<bool> {
        let now = Self::now().to_rfc3339();
        let decided = sqlx::query(
            "UPDATE pairing_requests \
             SET status = ?, decided_at = ?, decided_by = ?, profile_id = COALESCE(?, profile_id) \
             WHERE id = ? AND status = 'pending' AND expires_at > ?",
        )
        .bind(if accept { "approved" } else { "declined" })
        .bind(&now)
        .bind(by_device)
        .bind(profile_id)
        .bind(request_id)
        .bind(&now)
        .execute(&self.pool)
        .await
        .context("answering a pairing request")?;
        Ok(decided.rows_affected() > 0)
    }

    /// The request, if `claim_secret` is the one its phone was handed.
    async fn own_request(
        &self,
        request_id: &str,
        claim_secret: &str,
    ) -> Result<Option<sqlx::sqlite::SqliteRow>> {
        let row = sqlx::query(
            "SELECT id, client_id, client_type, device_name, profile_id, claim_hash, status, \
                    expires_at FROM pairing_requests WHERE id = ?",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await?;
        let asked = Self::sha256_hex(claim_secret.as_bytes());
        Ok(row.filter(|row| {
            let stored: String = row.get("claim_hash");
            bool::from(stored.as_bytes().ct_eq(asked.as_bytes()))
        }))
    }

    pub(super) async fn claim_request(
        &self,
        request_id: &str,
        claim_secret: &str,
    ) -> Result<PairingClaim> {
        let Some(row) = self.own_request(request_id, claim_secret).await? else {
            return Ok(PairingClaim::Gone);
        };
        let status: String = row.get("status");
        let expired = row.get::<String, _>("expires_at") <= Self::now().to_rfc3339();
        match status.as_str() {
            "declined" => return Ok(PairingClaim::Declined),
            "pending" if expired => return Ok(PairingClaim::Gone),
            "pending" => return Ok(PairingClaim::Waiting),
            "approved" => {}
            _ => return Ok(PairingClaim::Gone),
        }
        // Taken once: the row leaves `approved` before anything is minted, so two claims at
        // once cannot both be paired.
        let taken = sqlx::query(
            "UPDATE pairing_requests SET status = 'claimed' WHERE id = ? AND status = 'approved'",
        )
        .bind(request_id)
        .execute(&self.pool)
        .await?;
        if taken.rows_affected() == 0 {
            return Ok(PairingClaim::Gone);
        }
        let client_id: String = row.get("client_id");
        let client_type: String = row.get("client_type");
        let device_name: String = row.get("device_name");
        let profile_id: Option<String> = row.get("profile_id");
        let at = Self::now().to_rfc3339();
        self.register_device(
            &client_id,
            &device_name,
            &client_type,
            profile_id.as_deref(),
            &at,
        )
        .await;
        let (session, refresh, expires) = self
            .issue_session_pair(&client_id, &client_type, &client_id)
            .await?;
        tracing::info!(
            kind = "device_paired",
            client_id = %client_id,
            device = %device_name,
            method = "request",
            "a phone that was let in collected its pairing"
        );
        Ok(PairingClaim::Paired(PairedTokens {
            session_token: session,
            refresh_token: refresh,
            expires_at: expires.to_rfc3339(),
            device_name,
        }))
    }

    pub(super) async fn recover_request(
        &self,
        request_id: &str,
        claim_secret: &str,
        pin: &str,
    ) -> Result<PinCheck> {
        // The request first: a PIN is not spent on a phone that never asked.
        let Some(row) = self.own_request(request_id, claim_secret).await? else {
            return Ok(PinCheck::NoPin);
        };
        if row.get::<String, _>("status") != "pending" {
            return Ok(PinCheck::NoPin);
        }
        let checked = self.check_pin(pin, Self::now()).await?;
        if checked == PinCheck::Accepted {
            let owner = self.lock_state().await?.owner_profile_id;
            self.decide_request(request_id, true, None, owner.as_deref())
                .await?;
        }
        Ok(checked)
    }
}
