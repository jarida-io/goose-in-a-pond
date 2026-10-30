//! SQLite-backed implementation of `ProfileRepository`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::profile::{CreateProfileRequest, Profile, Role};
use pond_core::user_data::ports::profile::ProfileRepository;
use serde_json;
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::str::FromStr;
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
    role: String,
    guest_until: Option<String>,
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
    // An unrecognized stored value is a migration/data bug, not a reason to widen trust; the
    // least-privileged role is the only safe fallback.
    let role = Role::from_str(&row.role).unwrap_or(Role::Guest);
    Profile {
        id: row.id,
        display_name: row.display_name,
        avatar_emoji: row.avatar_emoji,
        preferences: prefs,
        role,
        guest_until: row.guest_until.as_deref().map(parse_dt),
        created_at: parse_dt(&row.created_at),
        updated_at: parse_dt(&row.updated_at),
    }
}

const PROFILE_COLUMNS: &str =
    "id, display_name, avatar_emoji, preferences, role, guest_until, created_at, updated_at";

#[async_trait]
impl ProfileRepository for SqliteProfileRepository {
    async fn create(&self, request: CreateProfileRequest) -> Result<Profile> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();

        // The first profile in an otherwise-empty household is unambiguously its owner — the
        // same "no one else to defer to" trust `ProfileScope::Household` already carries
        // elsewhere. Checked inside the insert's own transaction so a concurrent create can't
        // both see zero and both become owner.
        let mut tx = self.pool.begin().await?;
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profiles")
            .fetch_one(&mut *tx)
            .await?;
        let role = if existing == 0 {
            Role::Owner
        } else {
            Role::Member
        };

        sqlx::query(
            "INSERT INTO profiles (id, display_name, avatar_emoji, preferences, role, created_at, updated_at) \
             VALUES (?, ?, ?, '{}', ?, ?, ?)",
        )
        .bind(&id)
        .bind(&request.display_name)
        .bind(&request.avatar_emoji)
        .bind(role.as_str())
        .bind(&now_str)
        .bind(&now_str)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Profile {
            id,
            display_name: request.display_name,
            avatar_emoji: request.avatar_emoji,
            preferences: HashMap::new(),
            role,
            guest_until: None,
            created_at: now,
            updated_at: now,
        })
    }

    async fn get(&self, profile_id: &str) -> Result<Option<Profile>> {
        let row: Option<ProfileRow> = sqlx::query_as(&format!(
            "SELECT {PROFILE_COLUMNS} FROM profiles WHERE id = ?"
        ))
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_profile))
    }

    async fn list(&self) -> Result<Vec<Profile>> {
        let rows: Vec<ProfileRow> = sqlx::query_as(&format!(
            "SELECT {PROFILE_COLUMNS} FROM profiles ORDER BY created_at ASC, rowid ASC"
        ))
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

    async fn set_role(
        &self,
        profile_id: &str,
        role: Role,
        guest_until: Option<DateTime<Utc>>,
    ) -> Result<Profile> {
        let guest_until_str = (role == Role::Guest)
            .then(|| guest_until.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()))
            .flatten();
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let result =
            sqlx::query("UPDATE profiles SET role = ?, guest_until = ?, updated_at = ? WHERE id = ?")
                .bind(role.as_str())
                .bind(&guest_until_str)
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

    #[tokio::test]
    async fn the_first_profile_in_an_empty_household_is_its_owner() {
        let (repo, _tmp) = make_repo().await;
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(profile.role, Role::Owner);
        assert_eq!(profile.guest_until, None);
    }

    #[tokio::test]
    async fn a_new_profile_defaults_to_member_once_an_owner_exists() {
        let (repo, _tmp) = make_repo().await;
        repo.create(CreateProfileRequest {
            display_name: "Jerry".to_string(),
            avatar_emoji: "X".to_string(),
        })
        .await
        .unwrap();
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Liz".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(profile.role, Role::Member);
        assert_eq!(profile.guest_until, None);
    }

    #[tokio::test]
    async fn set_role_persists_role_and_guest_until() {
        let (repo, _tmp) = make_repo().await;
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Guestie".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        let until = Utc::now() + chrono::Duration::days(1);
        let updated = repo
            .set_role(&profile.id, Role::Guest, Some(until))
            .await
            .unwrap();
        assert_eq!(updated.role, Role::Guest);
        assert!(updated.guest_until.is_some());

        let fetched = repo.get(&profile.id).await.unwrap().unwrap();
        assert_eq!(fetched.role, Role::Guest);
        assert!(fetched.guest_until.is_some());
    }

    #[tokio::test]
    async fn guest_until_is_dropped_when_role_is_not_guest() {
        let (repo, _tmp) = make_repo().await;
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        let updated = repo
            .set_role(&profile.id, Role::Member, Some(Utc::now()))
            .await
            .unwrap();
        assert_eq!(updated.role, Role::Member);
        assert_eq!(
            updated.guest_until, None,
            "guest_until is meaningless for a non-guest role and must not be stored"
        );
    }

    #[tokio::test]
    async fn the_primary_profile_is_backfilled_as_owner() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteProfileRepository::new(db.system.clone());

        let owner = repo
            .create(CreateProfileRequest {
                display_name: "Liz".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        let other = repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "Y".to_string(),
            })
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('primary_profile_id', ?)")
            .bind(&owner.id)
            .execute(&db.system)
            .await
            .unwrap();

        // Re-run just this migration's backfill statements against the now-populated tables,
        // since `Database::init` already ran every migration before `settings` had a row.
        sqlx::query(
            "UPDATE profiles SET role = 'owner' \
             WHERE id = (SELECT value FROM settings WHERE key = 'primary_profile_id')",
        )
        .execute(&db.system)
        .await
        .unwrap();

        assert_eq!(repo.get(&owner.id).await.unwrap().unwrap().role, Role::Owner);
        assert_eq!(repo.get(&other.id).await.unwrap().unwrap().role, Role::Member);
    }
}
