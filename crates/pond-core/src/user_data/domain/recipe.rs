use serde::{Deserialize, Serialize};

/// A saved Goose Recipe stored in the database.
///
/// Recipes are reusable automations expressed as Goose YAML, executed via
/// `POST /api/v1/recipes/{name}/run`. There is no MCP tool for
/// self-invocation by the agent; a recipe run is always caller-initiated.
///
/// Recipe YAML with the fields GIAP reads (see `RecipeYaml` in
/// `pond-api::routes` for the full parsed shape):
/// ```yaml
/// title: Morning Brief
/// description: Daily weather and schedule summary
/// prompt: Give me the weather in {{city}} and list any scheduled tasks for today.
/// parameters:
///   - key: city
///     requirement: required
/// extensions:
///   - type: builtin
///     name: weather
/// activities:
///   - "Give me my morning brief"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecipe {
    /// UUID primary key.
    pub id: String,
    /// Unique slug used in API paths (e.g. "morning_brief", "lock_doors").
    pub name: String,
    /// One-sentence description shown in the UI.
    pub description: String,
    /// Goose Recipe YAML content.
    pub yaml: String,
    pub active: bool,
    /// ISO datetime when this recipe was created.
    pub created_at: String,
}
