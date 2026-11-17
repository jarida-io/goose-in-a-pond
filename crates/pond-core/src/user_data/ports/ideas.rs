//! Where a member's ideas are kept, with the to-dos an idea became.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::idea::{Idea, IdeaState};

/// A to-do an idea's step became, as the to-do list keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaTodo {
    pub id: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait IdeaRepository: Send + Sync {
    /// The member's ideas that are not dismissed, newest first.
    async fn list(&self, profile_id: &str) -> Result<Vec<Idea>>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Idea>>;

    async fn add(&self, idea: &Idea) -> Result<()>;

    /// `false` when the member has no idea by that id.
    async fn set_state(
        &self,
        profile_id: &str,
        id: &str,
        state: IdeaState,
        task_id: Option<&str>,
    ) -> Result<bool>;

    /// One open to-do on the member's list, from an idea's step: the one already there when
    /// the list holds it.
    async fn add_todo(&self, profile_id: &str, idea_id: &str, text: &str) -> Result<IdeaTodo>;
}
