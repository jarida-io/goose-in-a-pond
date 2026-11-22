//! Where anchors, who they are shared with and their deliverables are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::anchor::{Anchor, AnchorState, Deliverable};

/// A read that takes a `profile_id` serves that member: it returns an anchor, or a deliverable
/// of one, only when they own it or it is shared with them. The reads without one are the
/// engine's and cross members.
#[async_trait]
pub trait AnchorRepository: Send + Sync {
    /// `false` when its owner already has an anchor under that title.
    async fn add(&self, anchor: &Anchor) -> Result<bool>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Anchor>>;

    /// The owner's anchor under this title key, in any state.
    async fn by_title(&self, profile_id: &str, title_key: &str) -> Result<Option<Anchor>>;

    /// The member's own and shared anchors in `states`: dated ones nearest first, then the
    /// undated, newest first.
    async fn list(&self, profile_id: &str, states: &[AnchorState]) -> Result<Vec<Anchor>>;

    /// Writes every field of an anchor but who it is shared with; `false` when its owner has
    /// none by that id.
    async fn save(&self, anchor: &Anchor) -> Result<bool>;

    /// Replaces who an anchor is shared with; `false` when `owner_id` does not own it.
    async fn set_shares(
        &self,
        owner_id: &str,
        anchor_id: &str,
        profile_ids: &[String],
    ) -> Result<bool>;

    /// Any member's anchor, for the engine.
    async fn find(&self, id: &str) -> Result<Option<Anchor>>;

    /// Every member's candidates created before `before`.
    async fn candidates_older_than(&self, before: DateTime<Utc>) -> Result<Vec<Anchor>>;

    /// Every member's active anchors whose end, or start when they have no end, is before
    /// `before`.
    async fn active_over_before(&self, before: DateTime<Utc>) -> Result<Vec<Anchor>>;

    /// `false` when the anchor already has a deliverable in that slot.
    async fn add_deliverable(&self, deliverable: &Deliverable) -> Result<bool>;

    async fn deliverable(&self, profile_id: &str, id: &str) -> Result<Option<Deliverable>>;

    /// An anchor's deliverables in the order their slots were queued; empty when the member
    /// may not read the anchor.
    async fn deliverables(&self, profile_id: &str, anchor_id: &str) -> Result<Vec<Deliverable>>;

    /// The member's shown deliverables they have not opened, newest first.
    async fn unseen(&self, profile_id: &str) -> Result<Vec<Deliverable>>;

    /// Any member's deliverable, for the engine.
    async fn find_deliverable(&self, id: &str) -> Result<Option<Deliverable>>;

    /// Any anchor's deliverables in the order their slots were queued, for the engine.
    async fn deliverables_of(&self, anchor_id: &str) -> Result<Vec<Deliverable>>;

    /// Writes every field of a stored deliverable; `false` when there is none by that id.
    async fn save_deliverable(&self, deliverable: &Deliverable) -> Result<bool>;

    /// Shown deliverables of active anchors whose `refresh_at` has passed, longest due first.
    async fn deliverables_due(&self, now: DateTime<Utc>) -> Result<Vec<Deliverable>>;
}
