use serde::{Deserialize, Serialize};

/// Extra system-prompt section, sent every turn while active; ordered by `sort_order`, then `key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptExtra {
    /// Unique key that identifies this instruction block (e.g. "home_rules", "language").
    pub key: String,
    pub instruction: String,
    pub active: bool,
    /// Determines injection order (lower = first). Default 0.
    pub sort_order: i32,
}
