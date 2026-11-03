//! Where Voice Match keeps its prints and the wake CNN's results on enrolment takes.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::voice_print::{VoicePrint, WakeTake};

#[async_trait]
pub trait VoicePrintRepository: Send + Sync {
    async fn add_print(&self, print: &VoicePrint) -> Result<()>;

    /// Every member's prints from `model_name`, oldest first.
    async fn prints(&self, model_name: &str) -> Result<Vec<VoicePrint>>;

    /// Removes a member's prints; the number removed.
    async fn delete_prints(&self, profile_id: &str) -> Result<u64>;

    async fn add_wake_take(&self, take: &WakeTake) -> Result<()>;

    /// The takes read for `phrase`, oldest first.
    async fn wake_takes(&self, phrase: &str) -> Result<Vec<WakeTake>>;

    /// Forgets every wake take: they belong to the phrase they were read for.
    async fn clear_wake_takes(&self) -> Result<u64>;

    /// Forgets one member's wake takes, when everything about them is forgotten.
    async fn delete_wake_takes(&self, profile_id: &str) -> Result<u64>;

    /// Forgets one member's wake templates with their voice, keeping what the CNN heard.
    async fn forget_wake_templates(&self, profile_id: &str) -> Result<u64>;
}
