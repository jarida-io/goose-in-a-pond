use crate::user_data::domain::recipe::AgentRecipe;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: Goose-compatible YAML recipes. Runs are caller-initiated only (no MCP tool).
#[async_trait]
pub trait AgentRecipeRepository: Send + Sync {
    /// Return all recipes, ordered by name.
    async fn list(&self) -> Result<Vec<AgentRecipe>>;

    /// Fetch a recipe by its unique name/slug.
    async fn get_by_name(&self, name: &str) -> Result<Option<AgentRecipe>>;

    /// Fetch a recipe by its UUID.
    async fn get_by_id(&self, id: &str) -> Result<Option<AgentRecipe>>;

    /// Insert or replace a recipe. `name` must be unique.
    async fn upsert(&self, recipe: &AgentRecipe) -> Result<()>;

    /// Delete a recipe by UUID.
    async fn delete(&self, id: &str) -> Result<()>;
}
