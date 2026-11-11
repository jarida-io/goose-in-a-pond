//! Queue of composed suggestions. Composing costs seconds of model time on the board, so an
//! idle pass on the inference lane fills it and rendering only reads rows.

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::services::suggestion_generation::GeneratedSuggestion;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedSuggestion {
    pub id: String,
    pub profile_id: Option<String>,
    pub prompt: String,
    pub reason: String,
    pub source_memory_id: String,
    /// The source memory's text, read live, which grounds the chat model's answer.
    pub fact: String,
    pub created_at: DateTime<Utc>,
}

impl QueuedSuggestion {
    /// Grounding context that originated this queued suggestion.
    pub fn context(&self) -> crate::user_data::domain::suggestion::SuggestionContext {
        crate::user_data::domain::suggestion::SuggestionContext::PersonalMemory {
            source_memory_id: self.source_memory_id.clone(),
            fact: self.fact.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Tapped: the prompt went to chat.
    Taken,
    /// Declined; the row stays so a later pass can see that.
    Dismissed,
}

impl Settled {
    pub fn as_str(self) -> &'static str {
        match self {
            Settled::Taken => "taken",
            Settled::Dismissed => "dismissed",
        }
    }
}

/// No default bodies, so a decorator can't inherit an `Ok(0)` that silently stores nothing.
#[async_trait]
pub trait SuggestionQueueRepository: Send + Sync {
    /// Returns how many rows were new; a memory that already has a live suggestion adds nothing.
    async fn queue(&self, suggestions: &[GeneratedSuggestion]) -> Result<usize>;

    /// Offerable rows, newest first: only those whose source memory is still live (a join, not
    /// an FK cascade), scoped by `scope_sql` so `Guest` sees nothing.
    async fn offerable(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<QueuedSuggestion>>;

    /// Record what the household did. `false` means no queued row by that id.
    async fn settle(&self, id: &str, outcome: Settled) -> Result<bool>;

    /// Memory ids with a live suggestion, which the generation pass skips rather than re-compose.
    async fn live_memory_ids(&self) -> Result<Vec<String>>;
}
