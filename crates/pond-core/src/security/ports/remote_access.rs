//! Durable removal of remote networking permission for an authenticated device.
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Queue network revocation before the caller discards its Pond credentials.
/// Implementations must persist the intent locally even when coordination is offline.
#[async_trait]
pub trait RemoteRevocation: Send + Sync {
    /// Record an authenticated device revocation; repeated calls are idempotent.
    async fn queue(&self, device_id: &str) -> Result<()>;
}

/// How long a device may keep remote access without returning to the household
/// LAN.
///
/// Remote access is granted to a device because it was, at some point, standing
/// in the house. Nothing afterwards re-checks that. A phone that is lost,
/// stolen, or belonged to someone who has since left keeps a working route into
/// the household indefinitely, and the owner has to notice and act for that to
/// stop.
///
/// Thirty days is long enough that ordinary travel does not cost somebody their
/// access while they are away -- which is exactly when they need it -- and short
/// enough that a device nobody brings home again does not stay reachable for a
/// year.
pub const LAN_PRESENCE_WINDOW_DAYS: i64 = 30;

/// Proof that a device is still part of this household, renewed by being on its
/// network.
///
/// This is a second, independent gate to local approval rather than a
/// replacement for it. Approval decides **who may change** a household's remote
/// identity, and is what stops somebody briefly on the wifi installing their
/// own. Presence decides **how long an identity stays valid unattended**, and is
/// what stops a device that never comes home keeping its route forever. Neither
/// covers the other's case.
#[async_trait]
pub trait DevicePresence: Send + Sync {
    /// Record that this device authenticated from the household LAN, now.
    ///
    /// Called on the request path, so it must not fail the request: a presence
    /// record that could not be written loses a renewal, and the worst outcome
    /// of that is a device asked to come home sooner than it needed to.
    async fn seen_on_lan(&self, device_id: &str);

    /// Devices whose last LAN sighting is older than `cutoff`.
    ///
    /// A device with no record at all is never returned. Absence of evidence is
    /// not evidence of absence, and revoking on it would punish a device this
    /// pond has simply never happened to observe.
    async fn absent_since(&self, cutoff: DateTime<Utc>) -> Result<Vec<String>>;

    /// When this device's remote access lapses if it does not return, so the
    /// app can say so before it happens rather than afterwards.
    async fn lapses_at(&self, device_id: &str) -> Result<Option<DateTime<Utc>>>;
}
