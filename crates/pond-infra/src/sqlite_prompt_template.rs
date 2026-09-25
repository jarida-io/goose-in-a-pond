//! SQLite `PromptTemplateRepository` over the `prompt_templates` table in `pond_system.db`.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::prompt_template::PromptTemplate;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;

pub struct SqlitePromptTemplateRepository {
    pool: Pool<Sqlite>,
}

impl SqlitePromptTemplateRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_to_template(row: &sqlx::sqlite::SqliteRow) -> Result<PromptTemplate> {
    Ok(PromptTemplate {
        name: row.try_get("name")?,
        content: row.try_get("content")?,
        description: row.try_get("description")?,
        is_system: row.try_get::<i64, _>("is_system")? != 0,
        is_customized: row.try_get::<i64, _>("is_customized")? != 0,
        factory_version: row.try_get::<i64, _>("factory_version")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[async_trait]
impl PromptTemplateRepository for SqlitePromptTemplateRepository {
    async fn get(&self, name: &str) -> Result<Option<PromptTemplate>> {
        let rows = sqlx::query(
            "SELECT name, content, description, is_system, is_customized, factory_version, \
             updated_at FROM prompt_templates WHERE name = ?",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;

        rows.first().map(row_to_template).transpose()
    }

    async fn list(&self) -> Result<Vec<PromptTemplate>> {
        let rows = sqlx::query(
            "SELECT name, content, description, is_system, is_customized, factory_version, \
             updated_at FROM prompt_templates ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_template).collect()
    }

    async fn upsert(&self, template: &PromptTemplate) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO prompt_templates \
             (name, content, description, is_system, is_customized, factory_version, \
              updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&template.name)
        .bind(&template.content)
        .bind(&template.description)
        .bind(template.is_system as i64)
        .bind(template.is_customized as i64)
        .bind(template.factory_version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_if_absent(&self, template: &PromptTemplate) -> Result<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO prompt_templates \
             (name, content, description, is_system, is_customized, factory_version, \
              updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&template.name)
        .bind(&template.content)
        .bind(&template.description)
        .bind(template.is_system as i64)
        .bind(template.is_customized as i64)
        .bind(template.factory_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn seed_system_template(&self, template: &PromptTemplate) -> Result<()> {
        sqlx::query(
            // Customized rows keep their old `factory_version`: the Prompts tab's update cue.
            "INSERT INTO prompt_templates \
             (name, content, description, is_system, is_customized, factory_version, \
              updated_at) \
             VALUES (?, ?, ?, ?, 0, ?, datetime('now')) \
             ON CONFLICT(name) DO UPDATE SET \
                 content = excluded.content, \
                 description = excluded.description, \
                 is_system = excluded.is_system, \
                 factory_version = excluded.factory_version, \
                 updated_at = excluded.updated_at \
             WHERE prompt_templates.is_customized = 0",
        )
        .bind(&template.name)
        .bind(&template.content)
        .bind(&template.description)
        .bind(template.is_system as i64)
        .bind(template.factory_version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM prompt_templates WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::prompt_template::FACTORY_VERSION;

    async fn repo() -> SqlitePromptTemplateRepository {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();
        SqlitePromptTemplateRepository::new(pool)
    }

    fn factory(name: &str, content: &str) -> PromptTemplate {
        factory_at(name, content, FACTORY_VERSION)
    }

    fn factory_at(name: &str, content: &str, factory_version: i64) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            content: content.to_string(),
            description: "d".to_string(),
            is_system: true,
            is_customized: false,
            factory_version,
            updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn seed_never_clobbers_a_customized_template() {
        let repo = repo().await;
        repo.seed_system_template(&factory("balanced", "factory v1"))
            .await
            .unwrap();

        let mut edited = repo.get("balanced").await.unwrap().unwrap();
        edited.content = "my custom prompt".to_string();
        edited.is_customized = true;
        repo.upsert(&edited).await.unwrap();

        repo.seed_system_template(&factory("balanced", "factory v2"))
            .await
            .unwrap();
        let after = repo.get("balanced").await.unwrap().unwrap();
        assert_eq!(after.content, "my custom prompt");
        assert!(after.is_customized);
        assert!(after.is_system);
    }

    #[tokio::test]
    async fn seed_updates_untouched_templates_and_inserts_missing_ones() {
        let repo = repo().await;
        repo.seed_system_template(&factory("balanced", "factory v1"))
            .await
            .unwrap();
        repo.seed_system_template(&factory("balanced", "factory v2"))
            .await
            .unwrap();
        assert_eq!(
            repo.get("balanced").await.unwrap().unwrap().content,
            "factory v2"
        );
    }

    /// `is_customized` is set only by an explicit Save; never re-derive it from content.
    #[tokio::test]
    async fn a_customized_row_keeps_its_generation_and_an_owned_one_moves() {
        let repo = repo().await;
        repo.seed_system_template(&factory_at("balanced", "gen 1", 1))
            .await
            .unwrap();
        repo.seed_system_template(&factory_at("concise", "gen 1", 1))
            .await
            .unwrap();

        let mut edited = repo.get("balanced").await.unwrap().unwrap();
        edited.content = "mine".to_string();
        edited.is_customized = true;
        repo.upsert(&edited).await.unwrap();

        repo.seed_system_template(&factory_at("balanced", "gen 2", 2))
            .await
            .unwrap();
        repo.seed_system_template(&factory_at("concise", "gen 2", 2))
            .await
            .unwrap();

        let mine = repo.get("balanced").await.unwrap().unwrap();
        assert_eq!(mine.content, "mine", "the edit was clobbered");
        assert_eq!(
            mine.factory_version, 1,
            "a customized row must keep the generation it was forked from — that \
             is what makes `is_customized && factory_version < FACTORY_VERSION` \
             mean 'there is a newer built-in you have not seen'"
        );

        let theirs = repo.get("concise").await.unwrap().unwrap();
        assert_eq!(theirs.content, "gen 2");
        assert_eq!(
            theirs.factory_version, 2,
            "a row the reseed owns must move with it, or every untouched install \
             would show the update notice forever"
        );
    }

    /// A reset is an upsert with `is_customized = false`.
    #[tokio::test]
    async fn reset_returns_the_row_to_factory_ownership() {
        let repo = repo().await;
        repo.seed_system_template(&factory("balanced", "factory v1"))
            .await
            .unwrap();
        let mut edited = repo.get("balanced").await.unwrap().unwrap();
        edited.content = "custom".to_string();
        edited.is_customized = true;
        repo.upsert(&edited).await.unwrap();

        repo.upsert(&factory("balanced", "factory v1"))
            .await
            .unwrap();
        repo.seed_system_template(&factory("balanced", "factory v3"))
            .await
            .unwrap();
        assert_eq!(
            repo.get("balanced").await.unwrap().unwrap().content,
            "factory v3"
        );
    }
}
