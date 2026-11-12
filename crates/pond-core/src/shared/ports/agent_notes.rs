//! AgentNotesRepository port: where Goose's own lessons and follow-ups are kept.

use anyhow::Result;
use async_trait::async_trait;

use crate::shared::domain::agent_note::{AgentNote, NoteKind};
use crate::user_data::domain::profile::ProfileScope;

#[async_trait]
pub trait AgentNotesRepository: Send + Sync {
    async fn add(&self, note: AgentNote) -> Result<()>;

    /// The notes of one kind that `scope` reaches, newest first: a member's own and the
    /// household's; every one for the household; none for a guest.
    async fn list(&self, scope: &ProfileScope, kind: NoteKind) -> Result<Vec<AgentNote>>;

    /// One owner's notes of one kind, newest first; `None` is the household's own.
    async fn owned(&self, profile_id: Option<&str>, kind: NoteKind) -> Result<Vec<AgentNote>>;

    /// Remove a note `scope` reaches. `false` when there was none to remove.
    async fn remove(&self, id: &str, scope: &ProfileScope) -> Result<bool>;
}
