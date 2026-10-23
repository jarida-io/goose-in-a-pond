//! Durable queue so a disconnected device gets targeted notifications when it next opens its
//! stream or asks `GET /notifications/pending`. Delivered rows are stamped, not deleted, so they
//! can be audited until the pruning pass removes them (`pond_infra::pruning`).

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

    /// Whether a row with this id is queued for this device, delivered or not. The stream asks
    /// it to skip the live copy of a household notice the device already has its own copy of.
    async fn is_queued_for(&self, id: &str, device_id: &str) -> Result<bool>;

    /// Mark one notification delivered, but only when it is `device_id`'s. `Ok(true)` when the
    /// row is that device's (acknowledging twice is still `true`); `Ok(false)` when no such row
    /// belongs to it, which a caller must not distinguish from "no such row" to its client.
    async fn acknowledge(&self, id: &str, device_id: &str) -> Result<bool>;
}
