//! What the pond remembers about each agent's compiled prefill: the definition it was compiled
//! from, and when it was last compiled and last used, so the start-up pass can skip idle agents.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefillRecord {
    /// The role's definition hash when it was last compiled; empty for an agent used before it
    /// was ever compiled, which reads as changed.
    pub definition_hash: String,
    pub compiled_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}
