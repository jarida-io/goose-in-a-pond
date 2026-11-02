use anyhow::Result;
use async_trait::async_trait;

use crate::security::domain::admin_card::AdminPassword;

/// What a password is to the card in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardCheck {
    /// The card in force; `card_id` changes when the card is rotated.
    Matches {
        card_id: String,
    },
    Mismatch,
    /// No card was ever made on this pond.
    Missing,
}

/// Where a pond's admin card verifier lives. The pond only reads it: the card is made on the
/// device by `giap.sh admin-card`, which also seals the plaintext for recovery.
#[async_trait]
pub trait AdminCardStore: Send + Sync {
    /// The id of the card in force, `None` when no card was ever made. An unreadable record is
    /// an error, never `None`, so a damaged file narrows access instead of hiding the card.
    async fn current(&self) -> Result<Option<String>>;

    /// Whether `password` is the card in force. Deliberately slow.
    async fn check(&self, password: &AdminPassword) -> Result<CardCheck>;
}
