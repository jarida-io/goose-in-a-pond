//! SQLite-backed implementation of `PromptExtraRepository`.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::prompt_extra::PromptExtra;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;

pub struct SqlitePromptExtraRepository {
    pool: Pool<Sqlite>,
}

impl SqlitePromptExtraRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    async fn list_where(&self, filter: &str, profile_id: Option<&str>) -> Result<Vec<PromptExtra>> {
        let sql = format!(
            "SELECT key, instruction, active, sort_order, profile_id \
             FROM prompt_extras WHERE {filter} ORDER BY sort_order ASC, key ASC"
        );
        let mut query = sqlx::query(&sql);
        if let Some(id) = profile_id {
            query = query.bind(id);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_extra).collect()
    }
}

fn row_to_extra(row: &sqlx::sqlite::SqliteRow) -> Result<PromptExtra> {
    Ok(PromptExtra {
        key: row.try_get("key")?,
        instruction: row.try_get("instruction")?,
        active: row.try_get::<i64, _>("active")? != 0,
        sort_order: row.try_get::<i64, _>("sort_order")? as i32,
        profile_id: row.try_get("profile_id")?,
    })
}

#[async_trait]
impl PromptExtraRepository for SqlitePromptExtraRepository {
    async fn list_active(&self) -> Result<Vec<PromptExtra>> {
        self.list_where("active = 1 AND profile_id IS NULL", None)
            .await
    }

    async fn list_all(&self) -> Result<Vec<PromptExtra>> {
        self.list_where("profile_id IS NULL", None).await
    }

    async fn list_for_profile(&self, profile_id: &str) -> Result<Vec<PromptExtra>> {
        self.list_where("profile_id = ?", Some(profile_id)).await
    }

    async fn list_all_members(&self) -> Result<Vec<PromptExtra>> {
        self.list_where("profile_id IS NOT NULL", None).await
    }

    async fn upsert(&self, extra: &PromptExtra) -> Result<()> {
        self.upsert_all(std::slice::from_ref(extra)).await
    }

    async fn upsert_all(&self, extras: &[PromptExtra]) -> Result<()> {
        // Delete then insert: the owner-and-key index is on an expression, which plain
        // INSERT OR REPLACE would not name as its conflict target.
        let mut tx = self.pool.begin().await?;
        for extra in extras {
            sqlx::query("DELETE FROM prompt_extras WHERE key = ? AND profile_id IS ?")
                .bind(&extra.key)
                .bind(&extra.profile_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO prompt_extras \
                 (key, instruction, active, sort_order, profile_id, updated_at) \
                 VALUES (?, ?, ?, ?, ?, datetime('now'))",
            )
            .bind(&extra.key)
            .bind(&extra.instruction)
            .bind(extra.active as i64)
            .bind(extra.sort_order as i64)
            .bind(&extra.profile_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM prompt_extras WHERE key = ? AND profile_id IS NULL")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_for_profile(&self, profile_id: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM prompt_extras WHERE key = ? AND profile_id = ?")
            .bind(key)
            .bind(profile_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_all_for_profile(&self, profile_id: &str) -> Result<u64> {
        // Explicit, not ON DELETE CASCADE alone: `PRAGMA foreign_keys` is per connection.
        let done = sqlx::query("DELETE FROM prompt_extras WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn note(profile: Option<&str>, key: &str, instruction: &str) -> PromptExtra {
        PromptExtra {
            key: key.to_string(),
            instruction: instruction.to_string(),
            active: true,
            sort_order: 0,
            profile_id: profile.map(str::to_string),
        }
    }

    async fn repo() -> (SqlitePromptExtraRepository, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for id in ["amina", "otieno"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&db.system)
                .await
                .unwrap();
        }
        (SqlitePromptExtraRepository::new(db.system.clone()), tmp)
    }

    #[tokio::test]
    async fn household_and_member_notes_with_one_key_are_separate_rows() {
        let (repo, _tmp) = repo().await;
        repo.upsert(&note(None, "food", "Cook halal for guests."))
            .await
            .unwrap();
        repo.upsert(&note(Some("amina"), "food", "No pork."))
            .await
            .unwrap();
        repo.upsert(&note(Some("otieno"), "food", "Vegetarian."))
            .await
            .unwrap();
        repo.upsert(&note(Some("amina"), "food", "No pork or shellfish."))
            .await
            .unwrap();

        let household = repo.list_active().await.unwrap();
        assert_eq!(
            household,
            vec![note(None, "food", "Cook halal for guests.")]
        );
        assert_eq!(
            repo.list_for_profile("amina").await.unwrap(),
            vec![note(Some("amina"), "food", "No pork or shellfish.")]
        );
        assert_eq!(repo.list_all_members().await.unwrap().len(), 2);

        repo.delete("food").await.unwrap();
        assert!(repo.list_all().await.unwrap().is_empty());
        assert_eq!(
            repo.list_all_members().await.unwrap().len(),
            2,
            "household delete only"
        );
        repo.delete_for_profile("otieno", "food").await.unwrap();
        assert!(repo.list_for_profile("otieno").await.unwrap().is_empty());
        assert_eq!(repo.delete_all_for_profile("amina").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn upsert_all_lands_every_note() {
        let (repo, _tmp) = repo().await;
        repo.upsert_all(&[
            note(Some("amina"), "me_address", "Call her Mama Amina."),
            note(Some("amina"), "me_food", "No pork."),
        ])
        .await
        .unwrap();
        assert_eq!(repo.list_for_profile("amina").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_household_key_stays_unique() {
        let (repo, _tmp) = repo().await;
        let pool = &repo.pool;
        sqlx::query("INSERT INTO prompt_extras (key, instruction) VALUES ('k', 'a')")
            .execute(pool)
            .await
            .unwrap();
        assert!(
            sqlx::query("INSERT INTO prompt_extras (key, instruction) VALUES ('k', 'b')")
                .execute(pool)
                .await
                .is_err(),
            "NULL owners must collide, which a plain UNIQUE(profile_id, key) would not do"
        );
    }
}
