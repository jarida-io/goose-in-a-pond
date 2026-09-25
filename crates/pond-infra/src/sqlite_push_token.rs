//! SQLite-backed `PushTokenRepository` over `push_tokens`: one current token per device.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::user_data::domain::push_token::{PushPlatform, PushToken};
use pond_core::user_data::ports::push_token::PushTokenRepository;
use sqlx::{Pool, Sqlite};

pub struct SqlitePushTokenRepository {
    pool: Pool<Sqlite>,
}

impl SqlitePushTokenRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct PushTokenRow {
    device_id: String,
    token: String,
    platform: String,
    updated_at: String,
}

fn row_to_token(row: PushTokenRow) -> Result<PushToken> {
    let platform = PushPlatform::parse(&row.platform)
        .ok_or_else(|| anyhow!("invalid stored push platform: {:?}", row.platform))?;
    Ok(PushToken {
        device_id: row.device_id,
        token: row.token,
        platform,
        updated_at: row.updated_at,
    })
}

#[async_trait]
impl PushTokenRepository for SqlitePushTokenRepository {
    async fn upsert(&self, token: PushToken) -> Result<()> {
        sqlx::query(
            "INSERT INTO push_tokens (device_id, token, platform, updated_at) \
             VALUES (?, ?, ?, datetime('now')) \
             ON CONFLICT(device_id) DO UPDATE SET \
                 token = excluded.token, \
                 platform = excluded.platform, \
                 updated_at = datetime('now')",
        )
        .bind(token.device_id)
        .bind(token.token)
        .bind(token.platform.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, device_id: &str) -> Result<Option<PushToken>> {
        let row: Option<PushTokenRow> = sqlx::query_as(
            "SELECT device_id, token, platform, updated_at FROM push_tokens WHERE device_id = ?",
        )
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_token).transpose()
    }

    async fn list(&self) -> Result<Vec<PushToken>> {
        let rows: Vec<PushTokenRow> = sqlx::query_as(
            "SELECT device_id, token, platform, updated_at FROM push_tokens \
             ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_token).collect()
    }

    async fn delete(&self, device_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM push_tokens WHERE device_id = ?")
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    /// Fresh repo with one seeded device row (FK target for push_tokens).
    async fn fresh() -> SqlitePushTokenRepository {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO devices (id, name) VALUES ('dev-1', 'Phone')")
            .execute(&db.system)
            .await
            .unwrap();
        let repo = SqlitePushTokenRepository::new(db.system.clone());
        std::mem::forget(tmp); // keep the sqlite file alive for the test
        repo
    }

    fn tok(device: &str, token: &str, platform: PushPlatform) -> PushToken {
        PushToken {
            device_id: device.into(),
            token: token.into(),
            platform,
            updated_at: String::new(), // server-assigned on write
        }
    }

    #[tokio::test]
    async fn upsert_get_list_delete_roundtrip() {
        let repo = fresh().await;
        assert!(repo.get("dev-1").await.unwrap().is_none());

        repo.upsert(tok("dev-1", "expo-abc", PushPlatform::Expo))
            .await
            .unwrap();
        let got = repo.get("dev-1").await.unwrap().unwrap();
        assert_eq!(got.token, "expo-abc");
        assert_eq!(got.platform, PushPlatform::Expo);
        assert!(!got.updated_at.is_empty(), "updated_at stamped on write");

        repo.upsert(tok("dev-1", "fcm-xyz", PushPlatform::Fcm))
            .await
            .unwrap();
        let got = repo.get("dev-1").await.unwrap().unwrap();
        assert_eq!(got.token, "fcm-xyz");
        assert_eq!(got.platform, PushPlatform::Fcm);
        assert_eq!(repo.list().await.unwrap().len(), 1);

        repo.delete("dev-1").await.unwrap();
        assert!(repo.get("dev-1").await.unwrap().is_none());
    }
}
