use anyhow::Result;
use async_trait::async_trait;

use crate::models::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};

/// Source of truth for role assignments; the settings KV table is a cache synced at startup.
#[async_trait]
pub trait ModelRepository: Send + Sync {
    // ── Catalog ───────────────────────────────────────────────────────────────

    /// Return all models, ordered by category then name.
    async fn list_all(&self) -> Result<Vec<ModelRecord>>;

    async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>>;

    /// Look up a model by its stable `"{category}/{name}"` id.
    async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>>;

    /// `INSERT OR REPLACE`; set `model.is_custom = true` first for user-added rows.
    async fn upsert(&self, model: &ModelRecord) -> Result<()>;

    /// Update only the `downloaded` flag (and `updated_at`) for a model.
    async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()>;

    /// Remove a row; `false` when there was none. Never touches a file.
    async fn delete(&self, id: &str) -> Result<bool>;

    // ── Role assignments ──────────────────────────────────────────────────────

    async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>>;

    async fn get_assignment(&self, role: &str) -> Result<Option<ModelRoleAssignment>>;

    /// Upsert. `role` is a `ModelRole::as_str()` name; `model_id` must be an existing `models.id`.
    async fn set_assignment(&self, role: &str, model_id: &str) -> Result<()>;

    /// Remove the assignment for a role (no-op if not set).
    async fn clear_assignment(&self, role: &str) -> Result<()>;

    // ── Rollback history ──────────────────────────────────────────────────────
    // A catalog that keeps no history offers no rollback; only a stored one overrides these.

    /// `model_id` just gave a real answer for `role`.
    async fn note_working(&self, _role: &str, _model_id: &str) -> Result<()> {
        Ok(())
    }

    /// `role` is switching to `new_model_id`: its last working model, unless that is the new
    /// one, becomes what a rollback returns to.
    async fn arm_rollback(&self, _role: &str, _new_model_id: &str) -> Result<()> {
        Ok(())
    }

    /// The model a rollback of `role` would return to, if any.
    async fn rollback_target(&self, _role: &str) -> Result<Option<String>> {
        Ok(None)
    }
}
