//! Persistent knowledge store backed by Goose's flat-file `MemoryServer`. Its calls are
//! synchronous fs I/O, so adapters must wrap each in `tokio::task::spawn_blocking`.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;

#[async_trait]
pub trait McpKnowledgePort: Send + Sync {
    /// `global = true` stores in the shared memory dir; `false` is session-local.
    async fn remember(
        &self,
        category: &str,
        data: &str,
        tags: &[String],
        global: bool,
    ) -> Result<()>;

    /// Entries for `category` as `{ entry_key → [lines] }`; `"*"` returns every category.
    async fn retrieve(&self, category: &str, global: bool) -> Result<HashMap<String, Vec<String>>>;

    async fn remove_category(&self, category: &str, global: bool) -> Result<()>;

    async fn remove_specific(&self, category: &str, content: &str, global: bool) -> Result<()>;

    /// Context built from all stored memories, prepended to the LLM system prompt.
    fn instructions(&self) -> String;
}
