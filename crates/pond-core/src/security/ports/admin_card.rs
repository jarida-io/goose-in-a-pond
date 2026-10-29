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

/// The keys the pond issued to phones: one per device, kept only as a hash, so a phone keeps My Pond
/// open behind its own biometrics without holding the household's card.
#[async_trait]
pub trait AdminDeviceKeys: Send + Sync {
    /// A new key for `device`, replacing any it had. The only time the key is seen.
    async fn issue(&self, device: &str) -> Result<String>;

    /// Whether `key` is `device`'s live key.
    async fn check(&self, device: &str, key: &str) -> Result<bool>;

    /// Whether `device` holds a live key.
    async fn holds(&self, device: &str) -> Result<bool>;

    /// `Ok(false)` if it held none.
    async fn revoke(&self, device: &str) -> Result<bool>;

    /// Every phone's key, as rotating the card does. Returns how many went.
    async fn revoke_all(&self) -> Result<u64>;
}
