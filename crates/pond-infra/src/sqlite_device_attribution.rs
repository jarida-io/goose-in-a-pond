//! SQLite-backed [`DeviceAttribution`]: `devices.profile_id`, joined to `push_tokens`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::user_data::domain::push_token::{PushPlatform, PushToken};
use pond_core::user_data::ports::device_attribution::{checked_profile_id, DeviceAttribution};
use sqlx::{Pool, Sqlite};

pub struct SqliteDeviceAttribution {
    pool: Pool<Sqlite>,
}

impl SqliteDeviceAttribution {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DeviceAttribution for SqliteDeviceAttribution {
    async fn set_device_profile(&self, device_id: &str, profile_id: Option<&str>) -> Result<()> {
        let profile_id = profile_id.map(checked_profile_id).transpose()?;
        let result = sqlx::query(
            "UPDATE devices SET profile_id = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(profile_id)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            // A zero-row write would otherwise look like success.
            return Err(anyhow!("no registered device with id {device_id:?}"));
        }
        Ok(())
    }

    async fn device_profile(&self, device_id: &str) -> Result<Option<String>> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT profile_id FROM devices WHERE id = ?")
                .bind(device_id)
                .fetch_optional(&self.pool)
                .await?;
        // No device and no member both answer `None`; the resolver treats either as unknown.
        Ok(row.flatten())
    }

    async fn devices_for_profile(&self, profile_id: &str) -> Result<Vec<String>> {
        let profile_id = checked_profile_id(profile_id)?;
        // NULL never equals a bound value, so unattributed (shared) devices stay out.
        let ids: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM devices WHERE profile_id = ? ORDER BY created_at DESC, id",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids.into_iter().map(|(id,)| id).collect())
    }

    async fn push_tokens_for_profile(&self, profile_id: &str) -> Result<Vec<PushToken>> {
        let profile_id = checked_profile_id(profile_id)?;
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT t.device_id, t.token, t.platform, t.updated_at \
             FROM push_tokens t JOIN devices d ON d.id = t.device_id \
             WHERE d.profile_id = ? \
             ORDER BY t.updated_at DESC",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(device_id, token, platform, updated_at)| {
                let platform = PushPlatform::parse(&platform)
                    .ok_or_else(|| anyhow!("invalid stored push platform: {platform:?}"))?;
                Ok(PushToken {
                    device_id,
                    token,
                    platform,
                    updated_at,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::TempDir;

    struct Fixture {
        attribution: SqliteDeviceAttribution,
        pool: Pool<Sqlite>,
        _tmp: TempDir,
    }

    /// Fixture with nothing attributed, as on every pond after migration 0043.
    async fn fixture() -> Fixture {
        let tmp = TempDir::new().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for (id, name) in [("liz", "Liz"), ("jerry", "Jerry")] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&db.system)
                .await
                .unwrap();
        }
        for (id, name) in [
            ("phone-liz", "Liz Phone"),
            ("phone-jerry", "Jerry Phone"),
            ("tablet", "Kitchen Tablet"),
        ] {
            sqlx::query("INSERT INTO devices (id, name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&db.system)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO push_tokens (device_id, token, platform) VALUES (?, ?, 'expo')",
            )
            .bind(id)
            .bind(format!("tok-{id}"))
            .execute(&db.system)
            .await
            .unwrap();
        }
        Fixture {
            attribution: SqliteDeviceAttribution::new(db.system.clone()),
            pool: db.system.clone(),
            _tmp: tmp,
        }
    }

    #[tokio::test]
    async fn a_device_starts_unattributed_and_can_be_claimed_and_released() {
        let f = fixture().await;
        assert_eq!(
            f.attribution.device_profile("phone-liz").await.unwrap(),
            None
        );

        f.attribution
            .set_device_profile("phone-liz", Some("liz"))
            .await
            .unwrap();
        assert_eq!(
            f.attribution.device_profile("phone-liz").await.unwrap(),
            Some("liz".to_string())
        );

        f.attribution
            .set_device_profile("phone-liz", None)
            .await
            .unwrap();
        assert_eq!(
            f.attribution.device_profile("phone-liz").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_profile_can_be_asked_for_its_devices_and_their_push_tokens() {
        let f = fixture().await;
        f.attribution
            .set_device_profile("phone-liz", Some("liz"))
            .await
            .unwrap();
        f.attribution
            .set_device_profile("phone-jerry", Some("jerry"))
            .await
            .unwrap();

        assert_eq!(
            f.attribution.devices_for_profile("liz").await.unwrap(),
            vec!["phone-liz".to_string()]
        );
        let tokens = f.attribution.push_tokens_for_profile("liz").await.unwrap();
        assert_eq!(
            tokens.iter().map(|t| t.token.as_str()).collect::<Vec<_>>(),
            vec!["tok-phone-liz"]
        );
        assert_eq!(tokens[0].platform, PushPlatform::Expo);

        // Vacuity control: the excluded rows really exist.
        let all: (i64,) = sqlx::query_as("SELECT count(*) FROM push_tokens")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(all.0, 3, "fixture must hold three tokens to exclude two");
    }

    #[tokio::test]
    async fn an_unattributed_device_belongs_to_nobody_not_to_everybody() {
        let f = fixture().await;
        f.attribution
            .set_device_profile("phone-liz", Some("liz"))
            .await
            .unwrap();
        // `tablet` is deliberately left unattributed.

        let devices = f.attribution.devices_for_profile("liz").await.unwrap();
        assert!(
            !devices.contains(&"tablet".to_string()),
            "the shared tablet is not Liz's device: {devices:?}"
        );
        let tokens = f.attribution.push_tokens_for_profile("liz").await.unwrap();
        assert!(
            tokens.iter().all(|t| t.device_id != "tablet"),
            "a targeted send must not reach an unclaimed screen: {tokens:?}"
        );

        // And a member with no devices reaches nobody rather than everybody.
        assert!(f
            .attribution
            .devices_for_profile("jerry")
            .await
            .unwrap()
            .is_empty());
        assert!(f
            .attribution
            .push_tokens_for_profile("jerry")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn deleting_a_member_releases_their_devices_and_keeps_the_phone_working() {
        let f = fixture().await;
        f.attribution
            .set_device_profile("phone-liz", Some("liz"))
            .await
            .unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&f.pool)
            .await
            .expect("deleting a member who owns a device must succeed");

        assert_eq!(
            f.attribution.device_profile("phone-liz").await.unwrap(),
            None
        );
        let still_there: (i64,) =
            sqlx::query_as("SELECT count(*) FROM devices WHERE id = 'phone-liz'")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(still_there.0, 1, "the device outlives its owner");
        let token_survives: (i64,) =
            sqlx::query_as("SELECT count(*) FROM push_tokens WHERE device_id = 'phone-liz'")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(token_survives.0, 1, "the phone still receives broadcasts");

        // No dangling reference anywhere in the database.
        let violations: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&f.pool)
            .await
            .unwrap();
        assert!(violations.is_empty(), "dangling FKs: {violations:?}");
    }

    #[tokio::test]
    async fn attributing_a_device_that_is_not_registered_is_an_error() {
        let f = fixture().await;
        let err = f
            .attribution
            .set_device_profile("no-such-device", Some("liz"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no-such-device"),
            "the failure must name the device: {err}"
        );
    }

    #[tokio::test]
    async fn a_device_cannot_be_attributed_to_a_member_who_does_not_exist() {
        let f = fixture().await;
        assert!(
            f.attribution
                .set_device_profile("phone-liz", Some("ghost"))
                .await
                .is_err(),
            "the foreign key must refuse an unknown member"
        );
    }

    #[tokio::test]
    async fn a_blank_profile_id_is_refused_on_both_directions() {
        let f = fixture().await;
        assert!(f
            .attribution
            .set_device_profile("phone-liz", Some("  "))
            .await
            .is_err());
        assert!(f.attribution.devices_for_profile("").await.is_err());
        assert!(f.attribution.push_tokens_for_profile("").await.is_err());
    }
}
