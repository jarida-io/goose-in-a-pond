//! Driven port: which paired devices get a durable copy of a household-wide notice.
//!
//! A broadcast reaches only the screens connected at that moment. A phone is usually asleep, so
//! a household notice (a schedule's result, a rule's alert, a pairing) is also queued for every
//! paired phone and woken through the push relay. Desktops and other clients see it live only.

use anyhow::Result;
use async_trait::async_trait;

/// The client type a phone running the companion app pairs as (`devices.device_type`).
pub const PHONE_CLIENT_TYPE: &str = "gotg";

#[async_trait]
pub trait NotificationRecipients: Send + Sync {
    /// Ids of the phones ([`PHONE_CLIENT_TYPE`]) that hold a live pairing (an unrevoked
    /// session token), oldest pairing first. A failed read is an error, never an empty list.
    async fn paired_phones(&self) -> Result<Vec<String>>;
}
