//! Where the questions Goose asks a member, and their answers, are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::question::{Question, QuestionPurpose};

/// Every read and write is one member's, but for `expire_due`, which is the engine's.
#[async_trait]
pub trait QuestionRepository: Send + Sync {
    /// Stores the questions their member was never asked before, by key, and returns those.
    async fn add_batch(&self, questions: &[Question]) -> Result<Vec<Question>>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Question>>;

    /// The member's open and waiting questions, oldest first.
    async fn open(&self, profile_id: &str) -> Result<Vec<Question>>;

    /// Every question of one batch, in any state, in the order it was asked.
    async fn in_batch(&self, profile_id: &str, batch_id: &str) -> Result<Vec<Question>>;

    /// Every question one chat asked, in any state, oldest first.
    async fn for_session(&self, profile_id: &str, session_id: &str) -> Result<Vec<Question>>;

    /// The member's questions in any state, newest first: what a new question may repeat.
    async fn recent(&self, profile_id: &str, limit: usize) -> Result<Vec<Question>>;

    /// Every question the member was asked for an anchor, in any state, oldest first.
    async fn for_anchor(&self, profile_id: &str, anchor_id: &str) -> Result<Vec<Question>>;

    /// The member's answered questions, newest answer first.
    async fn answered(&self, profile_id: &str, limit: usize) -> Result<Vec<Question>>;

    /// Writes a question's state, answer and times; `false` when the member has none by that
    /// id.
    async fn save(&self, question: &Question) -> Result<bool>;

    /// Stamps the open questions of a batch as pushed to the member.
    async fn mark_asked(&self, profile_id: &str, batch_id: &str, at: DateTime<Utc>) -> Result<()>;

    /// When the member's newest batch of `purpose` was written.
    async fn last_batch_at(
        &self,
        profile_id: &str,
        purpose: QuestionPurpose,
    ) -> Result<Option<DateTime<Utc>>>;

    /// Marks the open questions of a chat batch as waiting; returns how many.
    async fn set_waiting(&self, profile_id: &str, batch_id: &str) -> Result<u64>;

    /// Expires every member's open questions whose deadline has passed; returns how many.
    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64>;

    /// Expires the open and waiting questions of a chat that was deleted; returns how many.
    async fn expire_session(&self, session_id: &str, now: DateTime<Utc>) -> Result<u64>;
}
