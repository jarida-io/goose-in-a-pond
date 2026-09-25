//! Driven port: persistence for device push-notification tokens, one per device.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::push_token::PushToken;

#[async_trait]
pub trait PushTokenRepository: Send + Sync {
    /// Insert or replace the token for a device (one current token per device).
    async fn upsert(&self, token: PushToken) -> Result<()>;

    /// The current token for a device, if one is registered.
    async fn get(&self, device_id: &str) -> Result<Option<PushToken>>;

    /// Every registered token, attributed or not: the broadcast set. Targeted sends must
    /// use `DeviceAttribution::push_tokens_for_profile`.
    async fn list(&self) -> Result<Vec<PushToken>>;

    /// Remove a device's token (logout / unpair).
    async fn delete(&self, device_id: &str) -> Result<()>;
}
