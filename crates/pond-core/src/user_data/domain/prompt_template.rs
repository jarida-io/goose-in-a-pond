use serde::{Deserialize, Serialize};

/// A named system-prompt template. Placeholders: `{{assistant_name}}`, `{{user_name}}`,
/// `{{personality}}`, `{{timezone}}`, `{{location}}`, `{{prompt_addendum}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    /// Unique name / slug — e.g. "balanced", "concise", or a user-defined name.
    pub name: String,
    pub content: String,
    pub description: String,
    /// `true` = built-in template seeded at setup; cannot be deleted.
    pub is_system: bool,
    /// Set by a user save, so the startup reseed skips the row; only an explicit reset clears it.
    #[serde(default)]
    pub is_customized: bool,
    /// Built-in generation this row was seeded or forked from (`0` = predates the column); a
    /// customized row below [`FACTORY_VERSION`] is an edit of an older built-in.
    #[serde(default)]
    pub factory_version: i64,
    /// ISO datetime of the last update (SQLite `datetime('now')` format).
    pub updated_at: String,
}

/// Built-in template generation; bump when shipped templates change meaningfully (not typos).
pub const FACTORY_VERSION: i64 = 2;
