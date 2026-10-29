//! [`AdminDeviceKeys`] in `pond_system.db`. Only a SHA-256 of each key is stored: the key is 32
//! random bytes, so a slow hash would add nothing.

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use pond_core::security::ports::admin_card::AdminDeviceKeys;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;

pub struct SqliteAdminDeviceKeys {
    pool: SqlitePool,
}

impl SqliteAdminDeviceKeys {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn digest(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[async_trait]
impl AdminDeviceKeys for SqliteAdminDeviceKeys {
    async fn issue(&self, device: &str) -> Result<String> {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        sqlx::query(
            "INSERT INTO admin_device_keys (device_id, key_hash) VALUES (?, ?)
             ON CONFLICT(device_id) DO UPDATE SET key_hash = excluded.key_hash,
                                                  created_at = datetime('now')",
        )
        .bind(device)
        .bind(digest(&key))
        .execute(&self.pool)
        .await
        .with_context(|| format!("issuing an admin key to {device}"))?;
        Ok(key)
    }

    async fn check(&self, device: &str, key: &str) -> Result<bool> {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT key_hash FROM admin_device_keys WHERE device_id = ?")
                .bind(device)
                .fetch_optional(&self.pool)
                .await
                .context("reading an admin key")?;
        Ok(stored.is_some_and(|stored| stored.as_bytes().ct_eq(digest(key).as_bytes()).into()))
    }

    async fn holds(&self, device: &str) -> Result<bool> {
        let held: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM admin_device_keys WHERE device_id = ?")
                .bind(device)
                .fetch_optional(&self.pool)
                .await
                .context("reading an admin key")?;
        Ok(held.is_some())
    }

    async fn revoke(&self, device: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM admin_device_keys WHERE device_id = ?")
            .bind(device)
            .execute(&self.pool)
            .await
            .context("revoking an admin key")?;
        Ok(done.rows_affected() > 0)
    }

    async fn revoke_all(&self) -> Result<u64> {
        let done = sqlx::query("DELETE FROM admin_device_keys")
            .execute(&self.pool)
            .await
            .context("revoking every admin key")?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, SqlitePool, SqliteAdminDeviceKeys) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        for id in ["phone", "tablet"] {
            sqlx::query("INSERT INTO devices (id, name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        (tmp, pool.clone(), SqliteAdminDeviceKeys::new(pool))
    }

    #[tokio::test]
    async fn a_key_checks_for_its_own_device_only_and_is_stored_hashed() {
        let (_tmp, pool, keys) = store().await;
        let key = keys.issue("phone").await.unwrap();
        assert_eq!(key.len(), 43);
        assert!(keys.check("phone", &key).await.unwrap());
        assert!(!keys.check("tablet", &key).await.unwrap());
        assert!(!keys.check("phone", "guess").await.unwrap());
        let stored: String = sqlx::query_scalar("SELECT key_hash FROM admin_device_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!stored.contains(&key));
    }

    #[tokio::test]
    async fn a_new_key_replaces_the_old_one() {
        let (_tmp, _pool, keys) = store().await;
        let first = keys.issue("phone").await.unwrap();
        let second = keys.issue("phone").await.unwrap();
        assert_ne!(first, second);
        assert!(!keys.check("phone", &first).await.unwrap());
        assert!(keys.check("phone", &second).await.unwrap());
    }

    #[tokio::test]
    async fn revoking_one_or_all_and_removing_a_device_take_keys_away() {
        let (_tmp, pool, keys) = store().await;
        keys.issue("phone").await.unwrap();
        keys.issue("tablet").await.unwrap();
        assert!(keys.revoke("phone").await.unwrap());
        assert!(!keys.revoke("phone").await.unwrap());
        assert!(!keys.holds("phone").await.unwrap());
        assert!(keys.holds("tablet").await.unwrap());

        sqlx::query("DELETE FROM devices WHERE id = 'tablet'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !keys.holds("tablet").await.unwrap(),
            "the key went with its device"
        );

        keys.issue("phone").await.unwrap();
        assert_eq!(keys.revoke_all().await.unwrap(), 1);
        assert!(!keys.holds("phone").await.unwrap());
    }

    #[tokio::test]
    async fn a_key_for_a_device_the_pond_does_not_know_is_refused() {
        let (_tmp, _pool, keys) = store().await;
        assert!(keys.issue("stranger").await.is_err());
    }
}
