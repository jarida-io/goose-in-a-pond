//! Where a member's interests, their portrait and their interests interview are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::interest::{
    Interest, InterestInput, InterestPatch, MemberPortrait, Observed, ObservedInterest, Origin,
};
use crate::user_data::domain::interest_interview::InterestInterview;
use crate::user_data::services::member_interview::MemberInterview;

/// Every read and write is one member's: no method reaches another member's rows.
#[async_trait]
pub trait InterestRepository: Send + Sync {
    /// Confirmed rows first, then guesses by evidence.
    async fn list(&self, profile_id: &str) -> Result<Vec<Interest>>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Interest>>;

    /// What a conversation showed: a new guess, or one more conversation behind an existing row.
    /// A conversation counts once per row however often it is read; a confirmed row changes
    /// nothing but its evidence; an avoided row never changes.
    async fn observe(
        &self,
        profile_id: &str,
        session_id: &str,
        seen: &ObservedInterest,
    ) -> Result<Observed>;

    /// A confirmed row, merged into any row with the same topic.
    async fn confirm(
        &self,
        profile_id: &str,
        input: &InterestInput,
        origin: Origin,
    ) -> Result<Interest>;

    /// `None` when the member has no row by that id.
    async fn patch(
        &self,
        profile_id: &str,
        id: &str,
        patch: &InterestPatch,
    ) -> Result<Option<Interest>>;

    /// `false` when the member has no row by that id.
    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool>;

    /// When a row the research reads last changed (confirmed, or a strong guess).
    async fn last_changed(&self, profile_id: &str) -> Result<Option<DateTime<Utc>>>;

    /// Rows no picture has been looked for yet, research-worthy first.
    async fn without_picture(&self, profile_id: &str, limit: usize) -> Result<Vec<Interest>>;

    /// The picture found for a row; `None` records that none was found, so it is not looked
    /// for again.
    async fn set_picture(&self, profile_id: &str, id: &str, key: Option<&str>) -> Result<()>;

    async fn portrait(&self, profile_id: &str) -> Result<Option<MemberPortrait>>;

    async fn save_portrait(&self, profile_id: &str, portrait: &MemberPortrait) -> Result<()>;

    /// `None` before the member ever started one.
    async fn interview(&self, profile_id: &str) -> Result<Option<InterestInterview>>;

    /// Stamps `updated_at` with the time of the write.
    async fn save_interview(&self, profile_id: &str, interview: &InterestInterview) -> Result<()>;

    /// The one structured interview; `None` before the member ever started it.
    async fn member_interview(&self, profile_id: &str) -> Result<Option<MemberInterview>>;

    /// Stamps `updated_at` with the time of the write.
    async fn save_member_interview(
        &self,
        profile_id: &str,
        interview: &MemberInterview,
    ) -> Result<()>;
}
