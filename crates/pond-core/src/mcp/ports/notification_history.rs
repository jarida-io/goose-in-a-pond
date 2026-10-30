//! Driven port: the household's notification history, which a screen that was not connected
//! when a notice went out (the dashboard after a reload) reads back, with its own read state.
//!
//! The queue (`notification_queue`) is a phone's inbox: per-device copies, acknowledged and then
//! pruned. The history is the household's record: one row per notice under its household id,
//! scoped by [`HistoryAudience`], kept as long as the queue keeps an undelivered copy.

use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::domain::notification_history::{HistoryAudience, HistoryEntry};
use crate::mcp::ports::notification::Notification;

#[async_trait]
pub trait NotificationHistory: Send + Sync {
    /// Record one durable notice for `audience`. Recording the same id again updates its content
    /// and keeps every device's read state; its audience is the one first recorded.
    async fn record(&self, notification: &Notification, audience: &HistoryAudience) -> Result<()>;

    /// The newest `limit` notices `device_id` may read ([`HistoryAudience::admits`]), newest
    /// first, each with that device's read state.
    async fn list_for_device(&self, device_id: &str, limit: usize) -> Result<Vec<HistoryEntry>>;

    /// Mark these notices read for `device_id`. An id the device may not read, or that does not
    /// exist, is skipped, never an error, so a client cannot learn which ids exist. Returns how
    /// many were newly marked.
    async fn mark_read(&self, device_id: &str, ids: &[String]) -> Result<u64>;
}
