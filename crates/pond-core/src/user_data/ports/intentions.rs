//! Where the heartbeat's intentions are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::intention::{ConditionKind, Intention, IntentionState};

/// A read that takes a `profile_id` is one member's; `waiting` and `waiting_past` are the
/// runner's and cross members.
#[async_trait]
pub trait IntentionRepository: Send + Sync {
    /// `false` when the member already has a waiting intention under that key.
    async fn add(&self, intention: &Intention) -> Result<bool>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Intention>>;

    /// The member's intentions in `state`, or all of them, newest first.
    async fn list(&self, profile_id: &str, state: Option<IntentionState>)
        -> Result<Vec<Intention>>;

    /// Every member's waiting intentions with this kind of condition, nearest deadline first.
    async fn waiting(&self, kind: ConditionKind) -> Result<Vec<Intention>>;

    /// Every member's waiting intentions whose deadline is before `now`.
    async fn waiting_past(&self, now: DateTime<Utc>) -> Result<Vec<Intention>>;

    /// Writes an intention's state, outcome and settle time; `false` when its member has none
    /// by that id.
    async fn save(&self, intention: &Intention) -> Result<bool>;
}
