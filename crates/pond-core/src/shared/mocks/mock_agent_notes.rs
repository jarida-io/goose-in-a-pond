//! In-memory `AgentNotesRepository` for tests.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::shared::domain::agent_note::{AgentNote, NoteKind};
use crate::shared::ports::agent_notes::AgentNotesRepository;
use crate::user_data::domain::profile::ProfileScope;

#[derive(Default)]
pub struct MockAgentNotes {
    notes: RwLock<Vec<AgentNote>>,
}

impl MockAgentNotes {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn all(&self) -> Vec<AgentNote> {
        self.notes.read().await.clone()
    }
}

fn reaches(scope: &ProfileScope, note: &AgentNote) -> bool {
    match scope {
        ProfileScope::Household => true,
        ProfileScope::Owner(id) => note.profile_id.as_deref().is_none_or(|owner| owner == id),
        ProfileScope::Guest => false,
    }
}

#[async_trait]
impl AgentNotesRepository for MockAgentNotes {
    async fn add(&self, note: AgentNote) -> Result<()> {
        self.notes.write().await.push(note);
        Ok(())
    }

    async fn list(&self, scope: &ProfileScope, kind: NoteKind) -> Result<Vec<AgentNote>> {
        let mut notes: Vec<AgentNote> = self
            .notes
            .read()
            .await
            .iter()
            .filter(|n| n.kind == kind && reaches(scope, n))
            .cloned()
            .collect();
        notes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(notes)
    }

    async fn owned(&self, profile_id: Option<&str>, kind: NoteKind) -> Result<Vec<AgentNote>> {
        let mut notes: Vec<AgentNote> = self
            .notes
            .read()
            .await
            .iter()
            .filter(|n| n.kind == kind && n.profile_id.as_deref() == profile_id)
            .cloned()
            .collect();
        notes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(notes)
    }

    async fn remove(&self, id: &str, scope: &ProfileScope) -> Result<bool> {
        let mut notes = self.notes.write().await;
        let before = notes.len();
        notes.retain(|n| !(n.id == id && reaches(scope, n)));
        Ok(notes.len() < before)
    }
}
