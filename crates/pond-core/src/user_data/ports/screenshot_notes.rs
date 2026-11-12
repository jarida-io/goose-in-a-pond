//! Where the one-line notes from a member's screenshots are kept. No picture is ever stored.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::services::screenshot_notes::ScreenshotNote;

/// Every read and write is one member's.
#[async_trait]
pub trait ScreenshotNoteRepository: Send + Sync {
    /// Keep a note, unless the member already has one saying the same; the oldest go once
    /// they hold more than `NOTES_KEPT`. `false` when it was a repeat.
    async fn add(&self, note: &ScreenshotNote) -> Result<bool>;

    /// The member's notes, newest first.
    async fn list(&self, profile_id: &str) -> Result<Vec<ScreenshotNote>>;

    /// Notes no idea was built on, written since `since`, newest first.
    async fn unused_since(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<ScreenshotNote>>;

    async fn mark_used(&self, profile_id: &str, id: &str, at: DateTime<Utc>) -> Result<()>;

    /// `false` when the member has no note by that id.
    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool>;

    /// Every note the member has; returns how many went.
    async fn delete_all(&self, profile_id: &str) -> Result<u64>;
}
