//! SQLite-backed implementation of `ProfileRepository`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::profile::{CreateProfileRequest, Profile};
use pond_core::user_data::ports::profile::ProfileRepository;
use serde_json;
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use uuid::Uuid;

pub struct SqliteProfileRepository {
    pool: Pool<Sqlite>,
}

impl SqliteProfileRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

// ── Row helper ────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct ProfileRow {
    id: String,
    display_name: String,
    avatar_emoji: String,
    preferences: String,
    created_at: String,
    updated_at: String,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn row_to_profile(row: ProfileRow) -> Profile {
    let prefs: HashMap<String, String> = serde_json::from_str(&row.preferences).unwrap_or_default();
    Profile {
        id: row.id,
        display_name: row.display_name,
        avatar_emoji: row.avatar_emoji,
        preferences: prefs,
        created_at: parse_dt(&row.created_at),
        updated_at: parse_dt(&row.updated_at),
    }
}

#[async_trait]
impl ProfileRepository for SqliteProfileRepository {
    async fn create(&self, request: CreateProfileRequest) -> Result<Profile> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();

        sqlx::query(
            "INSERT INTO profiles (id, display_name, avatar_emoji, preferences, created_at, updated_at) \
             VALUES (?, ?, ?, '{}', ?, ?)",
        )
        .bind(&id)
        .bind(&request.display_name)
        .bind(&request.avatar_emoji)
        .bind(&now_str)
        .bind(&now_str)
        .execute(&self.pool)
        .await?;

        Ok(Profile {
            id,
            display_name: request.display_name,
            avatar_emoji: request.avatar_emoji,
            preferences: HashMap::new(),
            created_at: now,
            updated_at: now,
        })
    }

    async fn get(&self, profile_id: &str) -> Result<Option<Profile>> {
        let row: Option<ProfileRow> =
            sqlx::query_as("SELECT id, display_name, avatar_emoji, preferences, created_at, updated_at FROM profiles WHERE id = ?")
                .bind(profile_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(row_to_profile))
    }

    async fn list(&self) -> Result<Vec<Profile>> {
        let rows: Vec<ProfileRow> =
            sqlx::query_as("SELECT id, display_name, avatar_emoji, preferences, created_at, updated_at FROM profiles ORDER BY created_at ASC, rowid ASC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(row_to_profile).collect())
    }

    async fn update_preferences(
        &self,
        profile_id: &str,
        prefs: HashMap<String, String>,
    ) -> Result<Profile> {
        let prefs_json = serde_json::to_string(&prefs)?;
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let result =
            sqlx::query("UPDATE profiles SET preferences = ?, updated_at = ? WHERE id = ?")
                .bind(&prefs_json)
                .bind(&now_str)
                .bind(profile_id)
                .execute(&self.pool)
                .await?;

        if result.rows_affected() == 0 {
            return Err(anyhow!("profile not found: {}", profile_id));
        }

        self.get(profile_id)
            .await?
            .ok_or_else(|| anyhow!("profile not found after update: {}", profile_id))
    }

    async fn delete(&self, profile_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM profiles WHERE id = ?")
            .bind(profile_id)
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

    async fn make_repo() -> (SqliteProfileRepository, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteProfileRepository::new(db.system), tmp)
    }

    #[tokio::test]
    async fn create_and_get() {
        let (repo, _tmp) = make_repo().await;
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "\u{1F986}".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(profile.display_name, "Jerry");

        let fetched = repo.get(&profile.id).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().display_name, "Jerry");
    }

    #[tokio::test]
    async fn list_returns_all() {
        let (repo, _tmp) = make_repo().await;
        repo.create(CreateProfileRequest {
            display_name: "A".to_string(),
            avatar_emoji: "A".to_string(),
        })
        .await
        .unwrap();
        repo.create(CreateProfileRequest {
            display_name: "B".to_string(),
            avatar_emoji: "B".to_string(),
        })
        .await
        .unwrap();
        assert_eq!(repo.list().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn update_preferences_persists() {
        let (repo, _tmp) = make_repo().await;
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        let mut prefs = HashMap::new();
        prefs.insert("language".to_string(), "sw".to_string());
        let updated = repo.update_preferences(&profile.id, prefs).await.unwrap();
        assert_eq!(
            updated.preferences.get("language").map(|s| s.as_str()),
            Some("sw")
        );
    }

    #[tokio::test]
    async fn delete_removes_profile() {
        let (repo, _tmp) = make_repo().await;
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Temp".to_string(),
                avatar_emoji: "T".to_string(),
            })
            .await
            .unwrap();
        repo.delete(&profile.id).await.unwrap();
        assert!(repo.get(&profile.id).await.unwrap().is_none());
    }
}
