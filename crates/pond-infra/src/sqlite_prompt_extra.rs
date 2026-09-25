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
}

fn row_to_extra(row: &sqlx::sqlite::SqliteRow) -> Result<PromptExtra> {
    Ok(PromptExtra {
        key: row.try_get("key")?,
        instruction: row.try_get("instruction")?,
        active: row.try_get::<i64, _>("active")? != 0,
        sort_order: row.try_get::<i64, _>("sort_order")? as i32,
    })
}

#[async_trait]
impl PromptExtraRepository for SqlitePromptExtraRepository {
    async fn list_active(&self) -> Result<Vec<PromptExtra>> {
        let rows = sqlx::query(
            "SELECT key, instruction, active, sort_order \
             FROM prompt_extras WHERE active = 1 \
             ORDER BY sort_order ASC, key ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_extra).collect()
    }

    async fn list_all(&self) -> Result<Vec<PromptExtra>> {
        let rows = sqlx::query(
            "SELECT key, instruction, active, sort_order \
             FROM prompt_extras ORDER BY sort_order ASC, key ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_extra).collect()
    }

    async fn upsert(&self, extra: &PromptExtra) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO prompt_extras \
             (key, instruction, active, sort_order, updated_at) \
             VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&extra.key)
        .bind(&extra.instruction)
        .bind(extra.active as i64)
        .bind(extra.sort_order as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM prompt_extras WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
