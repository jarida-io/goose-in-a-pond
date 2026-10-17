//! The pond's Wi-Fi station for each OS, behind `pond_core`'s `WifiStation` port.

use std::sync::Arc;

use pond_core::network::ports::wifi_station::WifiStation;

pub mod unsupported;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

/// This host's station: NetworkManager on Linux, CoreWLAN on macOS, refusal elsewhere.
pub fn platform() -> Arc<dyn WifiStation> {
    #[cfg(target_os = "linux")]
    return linux::platform();
    #[cfg(target_os = "macos")]
    return macos::platform();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Arc::new(unsupported::UnsupportedWifiStation::new(
        "this OS has no Wi-Fi adapter yet",
    ));
}

#[cfg(any(test, feature = "test-mocks"))]
pub use pond_core::network::mocks::mock_wifi_station::MockWifiStation;
