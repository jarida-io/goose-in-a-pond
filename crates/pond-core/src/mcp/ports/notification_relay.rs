//! Background push relay (FCM, APNs) via the device's stored push token. Best-effort: a missing
//! token or transient failure must not break foreground delivery.

use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::domain::notice_delivery::Wake;
use crate::mcp::ports::notification::Notification;

#[async_trait]
pub trait NotificationRelay: Send + Sync {
    /// Ask the device's push service to wake it for this notice. `Ok(Wake::NotSent)` when the
    /// pond had nothing to send with; `Err` when it tried and the service refused or was down.
    async fn relay(&self, notification: &Notification) -> Result<Wake>;
}
