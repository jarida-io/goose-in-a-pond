use std::sync::Arc;

use async_trait::async_trait;

use super::peripheral::{Advertisement, BleError, BlePeripheral, PeripheralStatus};
use crate::setup_channel::SetupChannel;

/// The peripheral on a host with no adapter: it says why, and refuses to start.
pub struct UnsupportedPeripheral {
    reason: String,
}

impl UnsupportedPeripheral {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl BlePeripheral for UnsupportedPeripheral {
    async fn start(&self, _channel: Arc<SetupChannel>) -> Result<(), BleError> {
        Err(BleError::Unavailable(self.reason.clone()))
    }

    async fn advertise(&self, _advertisement: Option<Advertisement>) -> Result<(), BleError> {
        Err(BleError::Unavailable(self.reason.clone()))
    }

    async fn shutdown(&self) {}

    fn status(&self) -> PeripheralStatus {
        PeripheralStatus::Unavailable {
            reason: self.reason.clone(),
        }
    }
}
