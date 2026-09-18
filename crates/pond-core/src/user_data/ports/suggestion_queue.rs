//! Port for the suggestions the pond composed, waiting to be offered.
//!
//! # Why a queue at all
//!
//! Composing a question out of a memory costs a model call, and Home is a
//! screen a household glances at from across a room. Generating at render time
//! would put a 2B model between somebody walking past the panel and the panel
//! saying anything — on a board where that call is measured in seconds. So the
//! pass runs on the inference lane while nobody is talking, and rendering reads
//! rows.
//!
//! # `offerable` is not `list`
//!
//! Two filters live in the adapter rather than at the call site, because both
//! are things a caller would forget exactly once:
//!
//! 1. **The source memory must still be live.** A queued question about a note
//!    the household has since deleted is an offer whose reason is false, and
//!    the reason is the only thing making the offer falsifiable. There is no FK
//!    doing this: a CASCADE would empty the queue silently, where a read-side
//!    join is visible and can be tested.
//! 2. **Scope.** `Guest` sees nothing, and the predicate is the same
//!    `scope_sql` every other personal read uses, so a suggestion composed from
//!    a member's note cannot reach a shared screen by a caller forgetting a
//!    check.
//!
//! # Settling is idempotent and says so
//!
//! [`settle`](SuggestionQueueRepository::settle) answers whether it changed a
//! row. A double tap on a touch panel is a real event, and "already taken" must
//! be distinguishable from "no such suggestion" — the first is the household
//! being quick and the second is a bug.

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::services::suggestion_generation::GeneratedSuggestion;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// A composed suggestion as it sits in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedSuggestion {
    pub id: String,
    pub profile_id: Option<String>,
    pub prompt: String,
    pub reason: String,
    pub source_memory_id: String,
    pub created_at: DateTime<Utc>,
}

/// What a household did with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// They tapped it. The prompt went to chat.
    Taken,
    /// They said no. The row stays so a later pass can see they did.
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

/// No default bodies. A decorator that forgot `queue` and inherited `Ok(0)`
/// would make every generation pass report success and store nothing — which is
/// precisely the shape of the defect this whole surface was built after, where
/// a reminder was counted, logged and never written.
#[async_trait]
pub trait SuggestionQueueRepository: Send + Sync {
    /// Store what a pass produced. Answers how many rows were NEW.
    ///
    /// Idempotent on the source memory: the unique index admits one live
    /// suggestion per memory, so a pass that re-proposes a question about a
    /// memory already queued adds nothing and says so rather than failing. That
    /// is what lets the pass run on every idle period without the queue growing
    /// a hundred variations of one note.
    async fn queue(&self, suggestions: &[GeneratedSuggestion]) -> Result<usize>;

    /// The rows that may be offered right now, newest first.
    ///
    /// Applies both filters in this port's docs. A caller gets a list it can
    /// render without further checking.
    async fn offerable(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<QueuedSuggestion>>;

    /// Record what the household did. `false` means no queued row by that id.
    async fn settle(&self, id: &str, outcome: Settled) -> Result<bool>;

    /// Memory ids that already carry a live suggestion.
    ///
    /// The generation pass subtracts these before choosing what to show the
    /// model, so a pass spends its twelve slots on notes nobody has been asked
    /// about — rather than composing a question the unique index will then
    /// refuse, which costs the same inference and yields nothing.
    async fn live_memory_ids(&self) -> Result<Vec<String>>;
}
