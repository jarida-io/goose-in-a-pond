//! The setup channel's radios as the household's status view reads them.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::network::domain::peripheral::PeripheralStatus;
use pond_core::network::domain::wifi::WifiCapabilities;
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::network::ports::wifi_station::WifiStation;

use crate::ble::BlePeripheral;
use crate::setup_channel::SetupChannel;

pub struct SetupRadioStatus {
    peripheral: Arc<dyn BlePeripheral>,
    wifi: Arc<dyn WifiStation>,
    channel: SetupChannel,
}

impl SetupRadioStatus {
    pub fn new(
        peripheral: Arc<dyn BlePeripheral>,
        wifi: Arc<dyn WifiStation>,
        channel: SetupChannel,
    ) -> Self {
        Self {
            peripheral,
            wifi,
            channel,
        }
    }
}

#[async_trait]
impl SetupRadios for SetupRadioStatus {
    fn ble(&self) -> PeripheralStatus {
        self.peripheral.status()
    }

    fn session_open(&self) -> bool {
        self.channel.is_session_open()
    }

    async fn wifi(&self) -> WifiCapabilities {
        self.wifi.capabilities().await
    }
}
