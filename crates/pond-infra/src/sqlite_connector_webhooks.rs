//! A member's webhook connectors (`connector_webhooks`, migration 0088). The token is the
//! credential a caller presents in the address, so only its SHA-256 is kept: the address is shown
//! once, when it is made, and a copy of the database cannot call it.

use anyhow::{Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

/// Random bytes in a token: 256 bits, past guessing.
const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct WebhookConnector {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_event_at: Option<DateTime<Utc>>,
}

/// A new, random token, safe in a URL path.
pub fn new_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The form a token is kept and looked up in.
pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether `token` could be one [`new_token`] made, checked before any lookup.
pub fn is_token_shaped(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct SqliteConnectorWebhooks {
    pool: Pool<Sqlite>,
}

fn time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("bad webhook timestamp {raw:?}"))
}

fn webhook_of(row: &SqliteRow) -> Result<WebhookConnector> {
    let last: Option<String> = row.try_get("last_event_at")?;
    Ok(WebhookConnector {
        id: row.try_get("id")?,
        owner: row.try_get("profile_id")?,
        name: row.try_get("name")?,
        created_at: time(&row.try_get::<String, _>("created_at")?)?,
        last_event_at: last.as_deref().map(time).transpose()?,
    })
}

impl SqliteConnectorWebhooks {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    /// Makes a webhook for `owner`; returns it and its token, which is never readable again.
    pub async fn create(
        &self,
        owner: &str,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<(WebhookConnector, String)> {
        let token = new_token();
        let hook = WebhookConnector {
            id: uuid::Uuid::new_v4().to_string(),
            owner: owner.to_string(),
            name: name.trim().to_string(),
            created_at: now,
            last_event_at: None,
        };
        sqlx::query(
            "INSERT INTO connector_webhooks (id, profile_id, name, token_hash, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&hook.id)
        .bind(&hook.owner)
        .bind(&hook.name)
        .bind(hash_token(&token))
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok((hook, token))
    }

    /// The webhook `token` opens, if any.
    pub async fn by_token(&self, token: &str) -> Result<Option<WebhookConnector>> {
        if !is_token_shaped(token) {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT id, profile_id, name, created_at, last_event_at FROM connector_webhooks \
             WHERE token_hash = ?",
        )
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(webhook_of).transpose()
    }

    /// The owner's webhooks, oldest first.
    pub async fn list_for(&self, owner: &str) -> Result<Vec<WebhookConnector>> {
        let rows = sqlx::query(
            "SELECT id, profile_id, name, created_at, last_event_at FROM connector_webhooks \
             WHERE profile_id = ? ORDER BY created_at, rowid",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(webhook_of).collect()
    }

    pub async fn touch(&self, id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE connector_webhooks SET last_event_at = ? WHERE id = ?")
            .bind(at.to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// `false` when the owner has no webhook by that id.
    pub async fn delete(&self, owner: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM connector_webhooks WHERE id = ? AND profile_id = ?")
            .bind(id)
            .bind(owner)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn a_token_is_long_random_and_kept_only_as_its_hash() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(is_token_shaped(&a), "{a}");
        assert_eq!(hash_token(&a).len(), 64);
        assert_eq!(hash_token(&a), hash_token(&a));
        assert_ne!(hash_token(&a), hash_token(&b));
        assert!(!is_token_shaped("short"));
        assert!(!is_token_shaped(&format!("{}/", &a[..42])));
    }

    #[tokio::test]
    async fn a_token_opens_its_webhook_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('jerry', 'Jerry')")
            .execute(&db.system)
            .await
            .unwrap();
        let hooks = SqliteConnectorWebhooks::new(db.system.clone());
        let now = Utc::now();
        let (hook, token) = hooks.create("jerry", " GitHub ", now).await.unwrap();
        assert_eq!(hook.name, "GitHub");
        let stored: String = sqlx::query_scalar("SELECT token_hash FROM connector_webhooks")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_ne!(stored, token, "the token itself is never kept");
        assert_eq!(hooks.by_token(&token).await.unwrap().unwrap().id, hook.id);
        assert!(hooks.by_token(&new_token()).await.unwrap().is_none());
        assert!(
            hooks.by_token(&stored).await.unwrap().is_none(),
            "the hash is no token"
        );
        hooks.touch(&hook.id, now).await.unwrap();
        let listed = hooks.list_for("jerry").await.unwrap();
        assert_eq!(
            listed[0].last_event_at.map(|t| t.timestamp()),
            Some(now.timestamp())
        );
        assert!(!hooks.delete("emmanuel", &hook.id).await.unwrap());
        assert!(hooks.delete("jerry", &hook.id).await.unwrap());
        assert!(hooks.by_token(&token).await.unwrap().is_none());
    }
}
