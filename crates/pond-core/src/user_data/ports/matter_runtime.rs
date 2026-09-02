//! Driven Port: Matter Runtime
//!
//! Turning Matter on used to be a startup-only decision: `serve()` read
//! `matter_enabled`, either connected to a controller or fell back to the
//! logging stub, and that choice stood until the process restarted. A user
//! flipping the setting therefore changed nothing until someone rebooted the
//! Pond — the same class of defect the mic gate fixed for privacy.
//!
//! This port is the seam that makes the decision reconcilable at runtime. The
//! API layer asks for a desired state and reads back what actually happened;
//! the adapter owns the controller process, the WebSocket, and the bridge
//! supervisor behind it.
//!
//! Reporting matters as much as switching. "Enabled" and "actually talking to a
//! controller" are different facts, and collapsing them into one `Option` is
//! what made an unreachable controller report itself as "Matter is not
//! enabled". [`MatterState`] keeps them apart so the user is told which of the
//! two is true.

use super::device_commissioning::DeviceCommissioningPort;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Where the Matter integration actually is, as opposed to what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MatterState {
    /// Turned off. Nothing is running and nothing is being attempted.
    Disabled,
    /// Enabled and converging: installing or starting the controller, or
    /// opening the WebSocket. The first enable on a fresh install downloads a
    /// controller's dependencies, so this can legitimately last minutes.
    Connecting,
    /// Enabled and connected — commissioning and device control are live.
    Connected,
    /// Enabled, but the controller could not be reached. Carries the failure so
    /// the user is shown the actual reason instead of a generic "off".
    Unreachable { error: String },
}

impl MatterState {
    /// Whether commissioning and Matter device control can be used right now.
    pub fn is_connected(&self) -> bool {
        matches!(self, MatterState::Connected)
    }
}

/// A snapshot of the runtime, safe to serialize straight to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterStatus {
    /// Whether the integration is meant to be running at all. On by default;
    /// not a user-facing toggle.
    pub enabled: bool,
    /// The controller URL currently in effect.
    pub url: String,
    /// What the runtime has managed to do about it.
    #[serde(flatten)]
    pub state: MatterState,
}

impl MatterStatus {
    /// The off state, used before anything has been attempted.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            state: MatterState::Disabled,
        }
    }
}

/// What the Matter integration is being asked to be.
///
/// A struct rather than a widening argument list. `apply(url, true)` at the call
/// site says nothing about what the flag turns on, and a second bool after it
/// would be worse — the field name is the documentation, and it travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatterConfig {
    /// WebSocket address of the controller. A loopback address is one GIAP will
    /// install and run itself; anything else is someone else's controller.
    pub url: String,
    /// Whether to ask the controller for a Bluetooth transport.
    ///
    /// Off by default, and worth the setting rather than always-on. BLE is how a
    /// device that has never been on the network is paired at all — out of its
    /// box it has no Wi-Fi credentials, so it cannot advertise on mDNS — but the
    /// radio needs a native module that may not be installed and permission a
    /// headless service does not have: `cap_net_raw` on Linux, and on macOS an
    /// `NSBluetoothAlwaysUsageDescription` in the bundle's Info.plist, without
    /// which the OS kills the process outright.
    pub ble: bool,
}

/// Driven Port: reconcile the Matter integration to a desired state.
#[async_trait]
pub trait MatterRuntimePort: Send + Sync {
    /// Request a desired state. Returns immediately — the runtime converges in
    /// the background and reports progress through [`status`](Self::status).
    ///
    /// Fire-and-forget on purpose: enabling can take minutes (controller
    /// install plus startup), and the settings write that triggers it must not
    /// block on that. Idempotent — asking for the state already in effect does
    /// nothing, so repeated saves do not churn the connection.
    fn apply(&self, config: MatterConfig);

    /// What the runtime is currently doing.
    async fn status(&self) -> MatterStatus;

    /// The live commissioner, or `None` unless [`MatterState::Connected`].
    async fn commissioner(&self) -> Option<Arc<dyn DeviceCommissioningPort>>;

    /// Tear everything down: stop the bridge and the controller GIAP started.
    /// Called on server shutdown, where leaving the controller running would
    /// orphan it beyond the Pond's lifetime.
    async fn shutdown(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_connected_permits_commissioning() {
        assert!(MatterState::Connected.is_connected());
        assert!(!MatterState::Disabled.is_connected());
        assert!(!MatterState::Connecting.is_connected());
        assert!(!MatterState::Unreachable {
            error: "refused".into()
        }
        .is_connected());
    }

    /// The UI branches on a flat `state` discriminant, and distinguishes
    /// "unreachable" from "off" by the error it carries — both have to survive
    /// serialization.
    #[test]
    fn status_serializes_flat_with_the_failure_reason() {
        let json = serde_json::to_value(MatterStatus {
            enabled: true,
            url: "ws://127.0.0.1:5580/giap".into(),
            state: MatterState::Unreachable {
                error: "connection refused".into(),
            },
        })
        .unwrap();

        assert_eq!(json["enabled"], true);
        assert_eq!(json["url"], "ws://127.0.0.1:5580/giap");
        assert_eq!(json["state"], "unreachable");
        assert_eq!(json["error"], "connection refused");
    }

    #[test]
    fn disabled_status_round_trips() {
        let json = serde_json::to_value(MatterStatus::disabled()).unwrap();
        assert_eq!(json["state"], "disabled");
        assert_eq!(json["enabled"], false);

        let back: MatterStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, MatterStatus::disabled());
    }
}
