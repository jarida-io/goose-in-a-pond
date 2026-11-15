//! Durable queue so a disconnected device gets targeted notifications when it next opens its
//! stream or asks `GET /notifications/pending`. Delivered rows are stamped, not deleted, so they
//! can be audited until the pruning pass removes them (`pond_infra::pruning`).

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::mcp::domain::notice_delivery::{DeliveryTotals, UnfetchedNotice};
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

    /// Persist a targeted notification as held: undelivered, and unseen by its phone, until
    /// [`take_held`](Self::take_held) hands it back. A queue that cannot hold just queues it.
    async fn enqueue_held(&self, notification: Notification) -> Result<()> {
        self.enqueue(notification).await
    }

    /// Remove and return every held notification, oldest first.
    async fn take_held(&self) -> Result<Vec<Notification>> {
        Ok(Vec::new())
    }

    /// Record that a push wake was sent for this copy.
    async fn note_wake(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// Record that these copies reached their phone, over the stream or a pending fetch. The
    /// first stamp is kept.
    async fn note_fetched(&self, _ids: &[String]) -> Result<()> {
        Ok(())
    }

    /// Record that `device_id` showed its copy to the person. `Ok(false)` as for `acknowledge`.
    async fn note_shown(&self, _id: &str, _device_id: &str) -> Result<bool> {
        Ok(false)
    }

    /// Record that the person opened `device_id`'s copy. `Ok(false)` as for `acknowledge`.
    async fn note_opened(&self, _id: &str, _device_id: &str) -> Result<bool> {
        Ok(false)
    }

    /// Record that this copy was said aloud in the room.
    async fn note_spoken(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// Copies queued for phones since `since` that no phone has fetched, oldest first.
    async fn list_unfetched(&self, _since: DateTime<Utc>) -> Result<Vec<UnfetchedNotice>> {
        Ok(Vec::new())
    }

    /// What became of the copies queued since `since`, test devices left out.
    async fn delivery_totals(&self, _since: DateTime<Utc>) -> Result<DeliveryTotals> {
        Ok(DeliveryTotals::default())
    }
}
