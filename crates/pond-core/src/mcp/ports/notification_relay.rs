//! Background push relay (FCM/APNs) via the device's stored push token. Best-effort: a missing
//! token or transient failure must not break foreground delivery.
//! TODO: real FCM/APNs HTTP client; the current adapter is a stub that logs the intent.

use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::ports::notification::Notification;

#[async_trait]
pub trait NotificationRelay: Send + Sync {
    /// Relay a targeted notification to its device's background push channel.
    async fn relay(&self, notification: &Notification) -> Result<()>;
}
