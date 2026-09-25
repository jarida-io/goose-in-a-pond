//! SQLite `AgentRecipeRepository` over the `agent_recipes` table in `pond_system.db`.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::recipe::AgentRecipe;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;

pub struct SqliteRecipeRepository {
    pool: Pool<Sqlite>,
}

impl SqliteRecipeRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_to_recipe(row: &sqlx::sqlite::SqliteRow) -> Result<AgentRecipe> {
    Ok(AgentRecipe {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        yaml: row.try_get("yaml")?,
        active: row.try_get::<i64, _>("active")? != 0,
        created_at: row.try_get("created_at")?,
    })
}

#[async_trait]
impl AgentRecipeRepository for SqliteRecipeRepository {
    async fn list(&self) -> Result<Vec<AgentRecipe>> {
        let rows = sqlx::query(
            "SELECT id, name, description, yaml, active, created_at \
             FROM agent_recipes ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_recipe).collect()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<AgentRecipe>> {
        let rows = sqlx::query(
            "SELECT id, name, description, yaml, active, created_at \
             FROM agent_recipes WHERE name = ?",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;

        rows.first().map(row_to_recipe).transpose()
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<AgentRecipe>> {
        let rows = sqlx::query(
            "SELECT id, name, description, yaml, active, created_at \
             FROM agent_recipes WHERE id = ?",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        rows.first().map(row_to_recipe).transpose()
    }

    async fn upsert(&self, recipe: &AgentRecipe) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO agent_recipes \
             (id, name, description, yaml, active, created_at) \
             VALUES (?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&recipe.id)
        .bind(&recipe.name)
        .bind(&recipe.description)
        .bind(&recipe.yaml)
        .bind(recipe.active as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM agent_recipes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
