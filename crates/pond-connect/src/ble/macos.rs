//! macOS: CoreBluetooth runs only in `pond-server ble-helper` ([`helper::run`]); the server process
//! supervises it through [`HelperPeripheral`] and never touches the radio itself.

use std::sync::Arc;

use super::helper::{HelperPeripheral, ProcessLauncher, RestartPolicy};
use super::peripheral::BlePeripheral;
use super::unsupported::UnsupportedPeripheral;

pub mod helper;

pub fn platform() -> Arc<dyn BlePeripheral> {
    match ProcessLauncher::current_exe() {
        Ok(launcher) => Arc::new(HelperPeripheral::new(
            Arc::new(launcher),
            RestartPolicy::default(),
        )),
        Err(error) => Arc::new(UnsupportedPeripheral::new(format!(
            "cannot find this binary to start the Bluetooth helper: {error}"
        ))),
    }
}
