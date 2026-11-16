//! Where what a member's screenshots and photos showed is kept. The pictures themselves are
//! files; a row only names its file.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::capture::Capture;

/// A read that takes a `profile_id` is one member's; `find`, `pending` and `pictures_due` are
/// the engine's and cross members.
#[async_trait]
pub trait CaptureRepository: Send + Sync {
    /// `false` when the member already sent a picture with that content hash.
    async fn add(&self, capture: &Capture) -> Result<bool>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Capture>>;

    /// The member's captures, newest first, without the ones held for the Budget specialist.
    async fn list(&self, profile_id: &str) -> Result<Vec<Capture>>;

    /// The member's money captures the Budget specialist has not read yet, oldest first.
    async fn hot(&self, profile_id: &str) -> Result<Vec<Capture>>;

    async fn count_pending(&self, profile_id: &str) -> Result<usize>;

    /// Writes every field of a stored capture; `false` when its member has none by that id.
    async fn save(&self, capture: &Capture) -> Result<bool>;

    /// Deletes the row and returns it, so the caller can delete its picture.
    async fn delete(&self, profile_id: &str, id: &str) -> Result<Option<Capture>>;

    /// Deletes every capture the member has and returns them.
    async fn delete_all(&self, profile_id: &str) -> Result<Vec<Capture>>;

    /// Any member's capture, for the engine.
    async fn find(&self, id: &str) -> Result<Option<Capture>>;

    /// Every member's captures waiting to be read, oldest first.
    async fn pending(&self, limit: usize) -> Result<Vec<Capture>>;

    /// Every member's captures whose picture's time is up.
    async fn pictures_due(&self, now: DateTime<Utc>) -> Result<Vec<Capture>>;
}
