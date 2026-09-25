//! SQLite `UserSkillRepository` over the `user_skills` table in `pond_system.db`.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::skill::UserSkill;
use pond_core::user_data::ports::skill::UserSkillRepository;

pub struct SqliteSkillRepository {
    pool: Pool<Sqlite>,
}

impl SqliteSkillRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_to_skill(row: &sqlx::sqlite::SqliteRow) -> Result<UserSkill> {
    Ok(UserSkill {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        icon: row.try_get("icon")?,
        content: row.try_get("content")?,
        active: row.try_get::<i64, _>("active")? != 0,
        created_at: row.try_get("created_at")?,
    })
}

#[async_trait]
impl UserSkillRepository for SqliteSkillRepository {
    async fn list_active(&self) -> Result<Vec<UserSkill>> {
        let rows = sqlx::query(
            "SELECT id, name, description, icon, content, active, created_at \
             FROM user_skills WHERE active = 1 ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_skill).collect()
    }

    async fn list_all(&self) -> Result<Vec<UserSkill>> {
        let rows = sqlx::query(
            "SELECT id, name, description, icon, content, active, created_at \
             FROM user_skills ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_skill).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<UserSkill>> {
        let rows = sqlx::query(
            "SELECT id, name, description, icon, content, active, created_at \
             FROM user_skills WHERE id = ?",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        rows.first().map(row_to_skill).transpose()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<UserSkill>> {
        let rows = sqlx::query(
            "SELECT id, name, description, icon, content, active, created_at \
             FROM user_skills WHERE name = ? AND active = 1",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;

        rows.first().map(row_to_skill).transpose()
    }

    async fn create(&self, skill: &UserSkill) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_skills (id, name, description, icon, content, active, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&skill.id)
        .bind(&skill.name)
        .bind(&skill.description)
        .bind(&skill.icon)
        .bind(&skill.content)
        .bind(skill.active as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update(&self, skill: &UserSkill) -> Result<()> {
        sqlx::query(
            "UPDATE user_skills SET name = ?, description = ?, icon = ?, content = ?, active = ? \
             WHERE id = ?",
        )
        .bind(&skill.name)
        .bind(&skill.description)
        .bind(&skill.icon)
        .bind(&skill.content)
        .bind(skill.active as i64)
        .bind(&skill.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM user_skills WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
