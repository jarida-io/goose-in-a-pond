//! Durable queue so a disconnected device gets targeted notifications when it next opens its
//! stream. Delivered rows are stamped, not deleted, so they can be audited.

use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::ports::notification::Notification;

#[async_trait]
pub trait NotificationQueueRepository: Send + Sync {
    /// Persist a targeted notification as undelivered.
    async fn enqueue(&self, notification: Notification) -> Result<()>;

    /// Undelivered notifications for a device, oldest first.
    async fn list_undelivered(&self, device_id: &str) -> Result<Vec<Notification>>;

    /// Mark the given notification ids as delivered (stamps `delivered_at`).
    async fn mark_delivered(&self, ids: &[String]) -> Result<()>;
}
