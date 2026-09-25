use crate::user_data::domain::prompt_template::PromptTemplate;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: named, user-editable system prompt templates.
#[async_trait]
pub trait PromptTemplateRepository: Send + Sync {
    /// Fetch a template by name. Returns `None` if not found.
    async fn get(&self, name: &str) -> Result<Option<PromptTemplate>>;

    /// List all templates, ordered by name.
    async fn list(&self) -> Result<Vec<PromptTemplate>>;

    /// Insert or replace a template; seeding must use `seed_system_template` instead.
    async fn upsert(&self, template: &PromptTemplate) -> Result<()>;

    /// Insert unless a row with that name exists; `true` if inserted.
    async fn insert_if_absent(&self, template: &PromptTemplate) -> Result<bool>;

    /// Seed a system template: insert if absent, update only while `is_customized = 0`.
    async fn seed_system_template(&self, template: &PromptTemplate) -> Result<()>;

    /// Delete a template by name; callers must refuse `is_system` ones from the public API.
    async fn delete(&self, name: &str) -> Result<()>;
}
