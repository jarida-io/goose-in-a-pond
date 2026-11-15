//! SQLite-backed implementation of `GuestDeviceAccess` over the `guest_device_access` table.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::ports::guest_device_access::GuestDeviceAccess;
use sqlx::{Pool, Sqlite};
use std::collections::HashSet;

pub struct SqliteGuestDeviceAccess {
    pool: Pool<Sqlite>,
}

impl SqliteGuestDeviceAccess {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl GuestDeviceAccess for SqliteGuestDeviceAccess {
    async fn allow(&self, profile_id: &str, device_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO guest_device_access (profile_id, device_id) VALUES (?, ?)",
        )
        .bind(profile_id)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn revoke(&self, profile_id: &str, device_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM guest_device_access WHERE profile_id = ? AND device_id = ?")
            .bind(profile_id)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn allowed_devices(&self, profile_id: &str) -> Result<HashSet<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT device_id FROM guest_device_access WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_profile::SqliteProfileRepository;
    use pond_core::user_data::domain::profile::CreateProfileRequest;
    use pond_core::user_data::ports::profile::ProfileRepository;
    use tempfile::tempdir;

    async fn make() -> (SqliteGuestDeviceAccess, SqliteProfileRepository, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (
            SqliteGuestDeviceAccess::new(db.system.clone()),
            SqliteProfileRepository::new(db.system.clone()),
            tmp,
        )
    }

    async fn guest(profiles: &SqliteProfileRepository) -> String {
        profiles
            .create(CreateProfileRequest {
                display_name: "Guestie".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn a_guest_starts_with_no_devices() {
        let (access, profiles, _tmp) = make().await;
        let g = guest(&profiles).await;
        assert!(access.allowed_devices(&g).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn allowing_then_revoking_a_device_round_trips() {
        let (access, profiles, _tmp) = make().await;
        let g = guest(&profiles).await;

        access.allow(&g, "lamp-1").await.unwrap();
        assert_eq!(
            access.allowed_devices(&g).await.unwrap(),
            HashSet::from(["lamp-1".to_string()])
        );

        access.revoke(&g, "lamp-1").await.unwrap();
        assert!(access.allowed_devices(&g).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn allowing_twice_is_idempotent() {
        let (access, profiles, _tmp) = make().await;
        let g = guest(&profiles).await;
        access.allow(&g, "lamp-1").await.unwrap();
        access.allow(&g, "lamp-1").await.unwrap();
        assert_eq!(access.allowed_devices(&g).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn two_guests_have_independent_allow_lists() {
        let (access, profiles, _tmp) = make().await;
        let a = guest(&profiles).await;
        let b = guest(&profiles).await;
        access.allow(&a, "lamp-1").await.unwrap();
        access.allow(&b, "lamp-2").await.unwrap();
        assert_eq!(
            access.allowed_devices(&a).await.unwrap(),
            HashSet::from(["lamp-1".to_string()])
        );
        assert_eq!(
            access.allowed_devices(&b).await.unwrap(),
            HashSet::from(["lamp-2".to_string()])
        );
    }
}
