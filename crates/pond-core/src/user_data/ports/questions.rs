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

    /// The member's open questions, oldest first.
    async fn open(&self, profile_id: &str) -> Result<Vec<Question>>;

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

    /// Expires every member's open questions whose deadline has passed; returns how many.
    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64>;
}
