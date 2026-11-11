//! Driven ports for watched links: where they are kept, how a product page is read, and how its
//! owner is told.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared::domain::cart::{CartItem, PricePoint, ProductReading};

#[async_trait]
pub trait CartRepository: Send + Sync {
    /// `false` when the owner already watches that address.
    async fn create(&self, item: &CartItem) -> Result<bool>;
    /// The owner's item by id; another member's is `None`.
    async fn get(&self, owner: &str, id: &str) -> Result<Option<CartItem>>;
    async fn find_by_url(&self, owner: &str, url: &str) -> Result<Option<CartItem>>;
    /// The owner's items, newest first.
    async fn list_for(&self, owner: &str) -> Result<Vec<CartItem>>;
    /// Items whose next check is at or before `now`, soonest first.
    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<CartItem>>;
    /// Writes every field but the id, the owner, the address and when it was made.
    async fn save(&self, item: &CartItem) -> Result<()>;
    async fn add_price(&self, id: &str, point: &PricePoint) -> Result<()>;
    /// The newest `limit` price points, oldest first.
    async fn history(&self, id: &str, limit: usize) -> Result<Vec<PricePoint>>;
    /// Forgets an item's prices, when the page changes currency.
    async fn clear_history(&self, id: &str) -> Result<()>;
    /// `false` when the owner has no item by that id.
    async fn delete(&self, owner: &str, id: &str) -> Result<bool>;
}

/// Whether a reading may ask the model, for a page whose structured data names no price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelUse {
    Allowed,
    Never,
}

/// Why a product page gave no reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductReadError {
    /// 401 or 403.
    Refused,
    /// 429.
    RateLimited,
    /// 404 or 410.
    Gone,
    /// Not on the public web; the Pond reads only public pages.
    NotPublic,
    /// Read, and no price or stock was found on it.
    NoPrice,
    Other(String),
}

impl ProductReadError {
    /// The reason in words, for the owner.
    pub fn words(&self) -> &'static str {
        match self {
            ProductReadError::Refused => "The shop would not let Goose read the page.",
            ProductReadError::RateLimited => "The shop asked Goose to wait before reading again.",
            ProductReadError::Gone => "The page is gone: the listing may have ended.",
            ProductReadError::NotPublic => "That address is not on the public web.",
            ProductReadError::NoPrice => "Goose could not find a price on the page.",
            ProductReadError::Other(_) => "The page could not be read.",
        }
    }
}

#[async_trait]
pub trait ProductReader: Send + Sync {
    async fn read(
        &self,
        url: &str,
        model: ModelUse,
    ) -> std::result::Result<ProductReading, ProductReadError>;
}

/// A notice for a watched link's owner.
#[derive(Debug, Clone, PartialEq)]
pub struct CartNotice {
    pub owner: String,
    pub cart_id: String,
    pub title: String,
    pub body: String,
    /// A drop or a target reached, which may also be said aloud to an owner who asked for that.
    pub spoken: bool,
}

#[async_trait]
pub trait CartNotifier: Send + Sync {
    async fn notify(&self, notice: CartNotice) -> Result<()>;
}
