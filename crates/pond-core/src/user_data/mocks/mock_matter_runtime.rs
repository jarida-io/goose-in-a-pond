//! `MatterRuntimePort` stub pinned to a chosen state, recording reconcile requests.

use crate::user_data::ports::device_commissioning::{
    CommissionedDevice, DeviceCommissioningPort, SetupCode,
};
use crate::user_data::ports::matter_runtime::{
    MatterConfig, MatterRuntimePort, MatterState, MatterStatus,
};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// Always-succeeding commissioner; the device is derived from the setup code.
pub struct StubCommissioner {
    /// Node id handed back for every commission call.
    pub node_id: u64,
}

#[async_trait]
impl DeviceCommissioningPort for StubCommissioner {
    async fn commission(
        &self,
        _code: SetupCode,
        name: Option<String>,
    ) -> Result<CommissionedDevice> {
        Ok(CommissionedDevice {
            device_id: format!("matter-{}", self.node_id),
            name: name.unwrap_or_else(|| "Light 1".into()),
            node_id: self.node_id,
            device_type: "light".into(),
            capabilities: vec!["power".into()],
        })
    }

    async fn decommission(&self, _node_id: u64) -> Result<()> {
        Ok(())
    }
}

/// A runtime fixed in one state; `apply` only records the request.
pub struct StubMatterRuntime {
    status: Mutex<MatterStatus>,
    commissioner: Option<Arc<dyn DeviceCommissioningPort>>,
    applied: Mutex<Vec<MatterConfig>>,
    shutdowns: Mutex<usize>,
}

impl StubMatterRuntime {
    /// Off — nothing enabled, no commissioner.
    pub fn disabled() -> Self {
        Self::in_state(false, MatterState::Disabled, None)
    }

    /// Enabled and connected, with a commissioner that succeeds.
    pub fn connected() -> Self {
        Self::in_state(
            true,
            MatterState::Connected,
            Some(Arc::new(StubCommissioner { node_id: 7 })),
        )
    }

    /// Enabled, still converging.
    pub fn connecting() -> Self {
        Self::in_state(true, MatterState::Connecting, None)
    }

    /// Enabled, but the controller could not be reached.
    pub fn unreachable(error: &str) -> Self {
        Self::in_state(
            true,
            MatterState::Unreachable {
                error: error.to_string(),
            },
            None,
        )
    }

    fn in_state(
        enabled: bool,
        state: MatterState,
        commissioner: Option<Arc<dyn DeviceCommissioningPort>>,
    ) -> Self {
        Self {
            status: Mutex::new(MatterStatus {
                enabled,
                url: "ws://127.0.0.1:5580/giap".into(),
                state,
            }),
            commissioner,
            applied: Mutex::new(Vec::new()),
            shutdowns: Mutex::new(0),
        }
    }

    /// Every URL the runtime was asked to converge to, in order.
    pub fn applied(&self) -> Vec<String> {
        self.applied
            .lock()
            .unwrap()
            .iter()
            .map(|config| config.url.clone())
            .collect()
    }

    /// Every request in full, for a test that cares about more than the URL.
    pub fn applied_configs(&self) -> Vec<MatterConfig> {
        self.applied.lock().unwrap().clone()
    }

    pub fn shutdown_count(&self) -> usize {
        *self.shutdowns.lock().unwrap()
    }
}

#[async_trait]
impl MatterRuntimePort for StubMatterRuntime {
    fn apply(&self, config: MatterConfig) {
        self.applied.lock().unwrap().push(config);
    }

    async fn status(&self) -> MatterStatus {
        self.status.lock().unwrap().clone()
    }

    async fn commissioner(&self) -> Option<Arc<dyn DeviceCommissioningPort>> {
        // Mirrors the real runtime: a commissioner exists only when connected.
        self.status
            .lock()
            .unwrap()
            .state
            .is_connected()
            .then(|| self.commissioner.clone())
            .flatten()
    }

    async fn shutdown(&self) {
        *self.shutdowns.lock().unwrap() += 1;
    }
}
