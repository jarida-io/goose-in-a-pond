//! Where a member's interests and their interests interview are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono_tz::Tz;

use crate::user_data::domain::interest::{
    Interest, InterestInput, InterestPatch, Observed, ObservedInterest, Origin,
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

    /// How many separate days, in `tz`, a conversation showed this interest.
    async fn sighted_days(&self, profile_id: &str, id: &str, tz: Tz) -> Result<u32>;

    /// The member's guesses worth asking them to confirm (`interest::ready_to_offer`): seen on
    /// three separate days in `tz`, or the subject of a news story they voted up.
    async fn ready_to_offer(&self, profile_id: &str, tz: Tz) -> Result<Vec<Interest>>;

    /// Rows no picture has been looked for yet, confirmed first.
    async fn without_picture(&self, profile_id: &str, limit: usize) -> Result<Vec<Interest>>;

    /// The picture found for a row; `None` records that none was found, so it is not looked
    /// for again.
    async fn set_picture(&self, profile_id: &str, id: &str, key: Option<&str>) -> Result<()>;

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
