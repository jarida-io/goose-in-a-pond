use crate::user_data::domain::profile_interview::Interview;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: each member's setup interview, at most one per member.
#[async_trait]
pub trait ProfileInterviewRepository: Send + Sync {
    async fn get(&self, profile_id: &str) -> Result<Option<Interview>>;

    /// The interview that runs in this chat session, if any.
    async fn get_by_session(&self, session_id: &str) -> Result<Option<Interview>>;

    /// Insert or replace the member's interview.
    async fn save(&self, interview: &Interview) -> Result<()>;

    async fn delete(&self, profile_id: &str) -> Result<()>;
}
