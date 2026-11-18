//! Where a member's idea cards are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::card::Card;

/// Every read and write is one member's, but for `purge_before`, which is the engine's.
#[async_trait]
pub trait CardRepository: Send + Sync {
    /// Stores a new set for the member; their new cards still on show fade at `now`.
    async fn add_set(&self, profile_id: &str, cards: &[Card], now: DateTime<Utc>) -> Result<()>;

    /// Stores a single card without fading existing new cards.
    async fn add_one(&self, card: &Card) -> Result<()>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Card>>;

    /// The cards the member sees: saved, and new ones not yet faded; newest set first, then by
    /// rank.
    async fn shown(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Card>>;

    /// Every card the member was offered since `since`, in any state, newest first.
    async fn since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<Card>>;

    /// Writes a card's state; `false` when the member has none by that id.
    async fn save(&self, card: &Card) -> Result<bool>;

    /// Deletes every unsaved card made before `before`; returns how many.
    async fn purge_before(&self, before: DateTime<Utc>) -> Result<u64>;
}
