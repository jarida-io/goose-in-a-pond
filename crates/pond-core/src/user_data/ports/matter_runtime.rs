//! Driven port: reconcile Matter on/off at runtime. [`MatterState`] keeps "enabled" apart
//! from "talking to a controller" so an unreachable controller isn't reported as off.

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
    /// Enabled and converging; the first enable downloads the controller, so minutes is normal.
    Connecting,
    /// Enabled and connected — commissioning and device control are live.
    Connected,
    /// Enabled, but the controller could not be reached; carries the reason for the user.
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
    /// Whether the integration should run at all; on by default, not a user-facing toggle.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatterConfig {
    /// Controller WebSocket address; loopback means GIAP installs and runs it itself.
    pub url: String,
    /// Ask the controller for BLE, needed to pair out-of-box devices. Off by default: Linux
    /// needs `cap_net_raw`; macOS kills it without `NSBluetoothAlwaysUsageDescription`.
    pub ble: bool,
}

/// Driven Port: reconcile the Matter integration to a desired state.
#[async_trait]
pub trait MatterRuntimePort: Send + Sync {
    /// Request a desired state; returns at once and converges in the background (see
    /// [`status`](Self::status)). Idempotent, so repeated saves don't churn the connection.
    fn apply(&self, config: MatterConfig);

    /// What the runtime is currently doing.
    async fn status(&self) -> MatterStatus;

    /// The live commissioner, or `None` unless [`MatterState::Connected`].
    async fn commissioner(&self) -> Option<Arc<dyn DeviceCommissioningPort>>;

    /// Stop the bridge and any controller GIAP started, so none outlives the Pond.
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

    /// The UI branches on the flat `state` tag and shows the carried error.
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
