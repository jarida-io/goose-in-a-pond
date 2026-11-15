//! A phone that stopped connecting is removed, its access with it, and the removal is written
//! where its member reads what the pond did by itself.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use pond_core::security::ports::handshake::Handshake;
use pond_core::shared::domain::goose_activity::ActivityKind;
use pond_core::shared::ports::goose_activity::GooseActivityRepository;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::phone_retirement::PhoneRetirement;
use pond_infra::db::Database;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_goose_activity::SqliteGooseActivity;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_notification_recipients::SqliteNotificationRecipients;
use pond_infra::sqlite_profile::SqliteProfileRepository;

struct RemoveAfter(u32);
#[async_trait]
impl SettingsRepository for RemoveAfter {
    async fn get(&self) -> Result<Settings> {
        let mut settings = Settings::default();
        settings.unseen_phone_remove_days = self.0;
        Ok(settings)
    }
    async fn update(&self, _: &Settings) -> Result<()> {
        Ok(())
    }
    async fn get_key(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_key(&self, _: &str, _: String) -> Result<()> {
        Ok(())
    }
}

async fn phone(pool: &sqlx::SqlitePool, id: &str, member: Option<&str>, days_ago: i64) {
    sqlx::query("INSERT INTO devices (id, name, device_type, profile_id) VALUES (?, ?, 'gotg', ?)")
        .bind(id)
        .bind(format!("{id}'s phone"))
        .bind(member)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO session_tokens (token_hash, refresh_hash, device_id, client_id, \
         client_type, created_at, expires_at, refresh_expires_at, last_seen_at) \
         VALUES (?, ?, ?, ?, 'gotg', '', '', '', ?)",
    )
    .bind(format!("hash-{id}"))
    .bind(format!("refresh-{id}"))
    .bind(id)
    .bind(id)
    .bind((Utc::now() - Duration::days(days_ago)).to_rfc3339())
    .execute(pool)
    .await
    .unwrap();
}

async fn retirement(pool: &sqlx::SqlitePool, days: u32) -> PhoneRetirement {
    PhoneRetirement::new(
        Arc::new(SqliteNotificationRecipients::new(pool.clone())),
        Arc::new(SqliteHandshakeAdapter::new(pool.clone(), None)),
        Arc::new(SqliteDeviceRegistry::new(pool.clone())),
        Arc::new(SqliteGooseActivity::new(pool.clone())),
        Arc::new(SqliteProfileRepository::new(pool.clone())),
        Arc::new(RemoveAfter(days)),
    )
}

#[tokio::test]
async fn a_phone_unseen_past_the_limit_is_removed_and_its_member_can_read_that() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let profiles = SqliteProfileRepository::new(pool.clone());
    let jerry = profiles
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "duck".into(),
        })
        .await
        .unwrap()
        .id;
    phone(&pool, "daily", Some(&jerry), 1).await;
    phone(&pool, "drawer", Some(&jerry), 40).await;
    phone(&pool, "harness", None, 60).await;

    assert_eq!(
        retirement(&pool, 0).await.pass(Utc::now()).await,
        0,
        "0 never removes"
    );
    assert_eq!(retirement(&pool, 30).await.pass(Utc::now()).await, 2);

    let registry = SqliteDeviceRegistry::new(pool.clone());
    assert!(registry.get_device("daily").await.unwrap().is_some());
    assert!(registry.get_device("drawer").await.unwrap().is_none());
    assert!(registry.get_device("harness").await.unwrap().is_none());
    let handshake = SqliteHandshakeAdapter::new(pool.clone(), None);
    assert_eq!(
        handshake.revoke_device("drawer").await.unwrap(),
        0,
        "its access went with it"
    );

    let told = SqliteGooseActivity::new(pool.clone())
        .list_for(&jerry, 10)
        .await
        .unwrap();
    assert_eq!(told.len(), 2, "his own phone, and the one bound to nobody");
    assert!(told
        .iter()
        .all(|entry| entry.kind == ActivityKind::PhoneRemoved));
    assert!(told.iter().any(|entry| entry.summary
        == "Removed the phone drawer's phone: it had not connected for 30 days"));

    assert_eq!(
        retirement(&pool, 30).await.pass(Utc::now()).await,
        0,
        "removed once"
    );
}
