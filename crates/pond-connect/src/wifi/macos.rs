//! The Mac's Wi-Fi station through CoreWLAN. CoreWLAN's calls block (a scan takes seconds), so
//! each runs on a blocking thread inside its own autorelease pool, and creates its own objects:
//! nothing CoreWLAN hands out crosses a thread.

use std::ffi::c_void;
use std::net::Ipv4Addr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::AnyThread;
use objc2_core_wlan::{CWErr, CWInterface, CWNetwork, CWSecurity, CWWiFiClient};
use objc2_foundation::{NSString, NSUInteger, NSUTF8StringEncoding};
use pond_core::network::domain::capability::Availability;
use pond_core::network::domain::wifi::{
    JoinedNetwork, NetworkState, NetworkStatus, WifiCapabilities, WifiJoinError, WifiNetwork,
    WifiPassphrase, WifiScanError, WifiSecurity, MAX_SIGNAL,
};
use pond_core::network::ports::wifi_station::WifiStation;
use tokio::time::Instant;
use zeroize::{Zeroize, Zeroizing};

use crate::interfaces;

/// How long a join waits after associating for DHCP to give the interface an IPv4 address.
const ADDRESS_WAIT: Duration = Duration::from_secs(30);
const ADDRESS_POLL: Duration = Duration::from_millis(500);

const NO_INTERFACE: &str = "this Mac has no Wi-Fi interface";
const POWERED_OFF: &str = "Wi-Fi is turned off on this Mac";
const NAMES_HIDDEN: &str =
    "macOS shows Wi-Fi network names only to apps with Location permission; \
     allow it for the app the pond runs under in System Settings > Privacy & Security > Location \
     Services";
const ADMIN_ONLY: &str = "this Mac asks for an administrator to change Wi-Fi networks \
     (System Settings > Wi-Fi > Advanced)";

pub fn platform() -> Arc<dyn WifiStation> {
    Arc::new(CoreWlanStation::default())
}

#[derive(Default)]
pub struct CoreWlanStation {
    /// One radio operation at a time; a second is `busy`.
    radio: tokio::sync::Mutex<()>,
    /// A scan came back with every name withheld, so the capability says why from then on.
    names_hidden: AtomicBool,
}

#[async_trait]
impl WifiStation for CoreWlanStation {
    async fn capabilities(&self) -> WifiCapabilities {
        let names_hidden = self.names_hidden.load(Ordering::Relaxed);
        blocking(move || probe(names_hidden))
            .await
            .unwrap_or_else(|| WifiCapabilities::unsupported("CoreWLAN did not answer"))
    }

    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
        let _radio = self.radio.try_lock().map_err(|_| WifiScanError::Busy)?;
        match blocking(scan_networks).await {
            Some(Ok(networks)) => Ok(networks),
            Some(Err(ScanFailure::NamesHidden)) => {
                self.names_hidden.store(true, Ordering::Relaxed);
                tracing::warn!(
                    kind = "wifi_names_hidden",
                    "macOS withheld every Wi-Fi name: the pond has no Location permission"
                );
                Err(WifiScanError::NotPermitted)
            }
            Some(Err(ScanFailure::Station(error))) => Err(error),
            None => Err(WifiScanError::Busy),
        }
    }

    async fn join(
        &self,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        let _radio = self.radio.try_lock().map_err(|_| WifiJoinError::Busy)?;
        let wanted = ssid.to_owned();
        // Our one copy, handed to macOS without another and wiped when macOS lets go of it.
        let secret = passphrase.map(|p| {
            let mut bytes = Zeroizing::new(Vec::with_capacity(p.expose().len()));
            bytes.extend_from_slice(p.expose().as_bytes());
            bytes
        });
        let interface_name = blocking(move || associate(&wanted, secret, hidden))
            .await
            .unwrap_or(Err(WifiJoinError::Timeout))?;
        tracing::info!(kind = "wifi_associated", interface = %interface_name, "joined a Wi-Fi network; waiting for an address");
        let deadline = Instant::now() + ADDRESS_WAIT;
        loop {
            if let Some(ipv4) = interfaces::ipv4_on(&interface_name) {
                return Ok(JoinedNetwork {
                    ssid: ssid.to_owned(),
                    ipv4,
                });
            }
            if Instant::now() >= deadline {
                return Err(WifiJoinError::NoAddress);
            }
            tokio::time::sleep(ADDRESS_POLL).await;
        }
    }

    async fn status(&self) -> NetworkStatus {
        blocking(read_status)
            .await
            .unwrap_or_else(NetworkStatus::disconnected)
    }
}

/// Runs `work` on a blocking thread inside an autorelease pool; `None` if that thread panicked.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(move || autoreleasepool(|_| work()))
        .await
        .ok()
}

fn interface() -> Option<Retained<CWInterface>> {
    // SAFETY: the shared client and its default interface are plain getters.
    unsafe { CWWiFiClient::sharedWiFiClient().interface() }
}

fn probe(names_hidden: bool) -> WifiCapabilities {
    let Some(interface) = interface() else {
        return WifiCapabilities::unsupported(NO_INTERFACE);
    };
    // SAFETY: plain property reads.
    let (powered, admin_only, associated, named) = unsafe {
        (
            interface.powerOn(),
            interface
                .configuration()
                .is_some_and(|c| c.requireAdministratorForAssociation()),
            interface.rssiValue() != 0,
            interface.ssid().is_some(),
        )
    };
    if !powered {
        return WifiCapabilities::unsupported(POWERED_OFF);
    }
    // Associated but nameless is the same withholding a scan would meet.
    let scan = if names_hidden || (associated && !named) {
        Availability::unavailable(NAMES_HIDDEN)
    } else {
        Availability::Available
    };
    let join = if admin_only {
        Availability::unavailable(ADMIN_ONLY)
    } else {
        Availability::Available
    };
    WifiCapabilities { scan, join }
}

enum ScanFailure {
    NamesHidden,
    Station(WifiScanError),
}

fn scan_networks() -> Result<Vec<WifiNetwork>, ScanFailure> {
    let interface = interface().ok_or(ScanFailure::Station(WifiScanError::Unsupported))?;
    // SAFETY: a synchronous scan on our own interface object.
    let found = unsafe { interface.scanForNetworksWithName_includeHidden_error(None, false) }
        .map_err(|error| {
            ScanFailure::Station(if error.code() == CWErr::CWOperationNotPermittedErr.0 {
                WifiScanError::NotPermitted
            } else {
                WifiScanError::Busy
            })
        })?
        .allObjects()
        .to_vec();
    let known = known_ssids(&interface);
    let mut networks = Vec::with_capacity(found.len());
    for network in &found {
        // SAFETY: plain property reads.
        let Some(ssid) = (unsafe { network.ssid() }) else {
            continue;
        };
        let ssid = ssid.to_string();
        networks.push(WifiNetwork {
            known: known.contains(&ssid),
            security: security(network),
            // SAFETY: a plain property read.
            signal: signal(unsafe { network.rssiValue() }),
            ssid,
        });
    }
    if networks.is_empty() && !found.is_empty() {
        return Err(ScanFailure::NamesHidden);
    }
    Ok(networks)
}

fn known_ssids(interface: &CWInterface) -> Vec<String> {
    // SAFETY: plain property reads.
    unsafe { interface.configuration() }
        .map(|configuration| {
            unsafe { configuration.networkProfiles() }
                .array()
                .to_vec()
                .iter()
                .filter_map(|profile| unsafe { profile.ssid() })
                .map(|ssid| ssid.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// -100 dBm and below is 0, -50 dBm and above is 100, linear between.
fn signal(rssi: isize) -> u8 {
    let percent = (rssi.saturating_add(100)).saturating_mul(2);
    percent.clamp(0, isize::from(MAX_SIGNAL)) as u8
}

fn security(network: &CWNetwork) -> WifiSecurity {
    // SAFETY: plain queries on a scanned network.
    let supports = |security: CWSecurity| unsafe { network.supportsSecurity(security) };
    if [
        CWSecurity::Enterprise,
        CWSecurity::WPAEnterprise,
        CWSecurity::WPAEnterpriseMixed,
        CWSecurity::WPA2Enterprise,
        CWSecurity::WPA3Enterprise,
        CWSecurity::DynamicWEP,
    ]
    .into_iter()
    .any(supports)
    {
        WifiSecurity::Enterprise
    } else if supports(CWSecurity::WPA3Personal) || supports(CWSecurity::WPA3Transition) {
        WifiSecurity::Wpa3
    } else if supports(CWSecurity::WPA2Personal) || supports(CWSecurity::Personal) {
        WifiSecurity::Wpa2
    } else if supports(CWSecurity::WPAPersonal) || supports(CWSecurity::WPAPersonalMixed) {
        WifiSecurity::Wpa
    } else if supports(CWSecurity::WEP) {
        WifiSecurity::Wep
    } else {
        // Open, and Enhanced Open (OWE), which asks for no passphrase either.
        WifiSecurity::Open
    }
}

/// Associates with the strongest access point of `ssid`; returns the interface to watch.
fn associate(
    ssid: &str,
    secret: Option<Zeroizing<Vec<u8>>>,
    hidden: bool,
) -> Result<String, WifiJoinError> {
    let interface = interface().ok_or(WifiJoinError::Unsupported)?;
    // SAFETY: plain calls on our own interface object.
    unsafe {
        if !interface.powerOn() && interface.setPower_error(true).is_err() {
            return Err(WifiJoinError::NotPermitted);
        }
    }
    let name = unsafe { interface.interfaceName() }
        .map(|name| name.to_string())
        .ok_or(WifiJoinError::Unsupported)?;
    // SAFETY: a synchronous directed scan on our own interface object.
    let candidates = unsafe {
        interface
            .scanForNetworksWithName_includeHidden_error(Some(&NSString::from_str(ssid)), hidden)
    }
    .map_err(|error| join_error(error.code(), false))?
    .allObjects()
    .to_vec();
    // SAFETY: plain property reads. A withheld name is trusted to CoreWLAN's own filter.
    let network = candidates
        .into_iter()
        .filter(|n| unsafe { n.ssid() }.is_none_or(|name| name.to_string() == ssid))
        .max_by_key(|n| unsafe { n.rssiValue() })
        .ok_or(WifiJoinError::NotFound)?;
    let had_secret = secret.is_some();
    let password = secret.map(passphrase_string).transpose()?;
    // SAFETY: associating our own interface with a network it just scanned.
    let joined =
        unsafe { interface.associateToNetwork_password_error(&network, password.as_deref()) };
    drop(password);
    joined.map_err(|error| join_error(error.code(), had_secret))?;
    Ok(name)
}

/// An NSString over our own buffer rather than a copy of it, whose deallocator zeroes the buffer
/// when macOS lets go of the string.
fn passphrase_string(mut secret: Zeroizing<Vec<u8>>) -> Result<Retained<NSString>, WifiJoinError> {
    if secret.is_empty() {
        return Err(WifiJoinError::NotPermitted);
    }
    let mut bytes = std::mem::ManuallyDrop::new(std::mem::take(&mut *secret));
    let (len, capacity) = (bytes.len(), bytes.capacity());
    let Some(data) = NonNull::new(bytes.as_mut_ptr().cast::<c_void>()) else {
        let mut bytes = std::mem::ManuallyDrop::into_inner(bytes);
        bytes.zeroize();
        return Err(WifiJoinError::NotPermitted);
    };
    let wipe = RcBlock::new(move |data: NonNull<c_void>, length: NSUInteger| {
        // SAFETY: NSString hands back exactly the buffer and length given below, once, when the
        // string is deallocated; `capacity` is that buffer's own.
        let mut buffer =
            unsafe { Vec::from_raw_parts(data.as_ptr().cast::<u8>(), length, capacity) };
        buffer.zeroize();
    });
    // SAFETY: `data` is valid for `len` bytes until the deallocator runs, and the string never
    // writes to it. The bytes are a Rust `String`'s, so UTF-8 decoding cannot fail.
    unsafe {
        NSString::initWithBytesNoCopy_length_encoding_deallocator(
            NSString::alloc(),
            data,
            len,
            NSUTF8StringEncoding,
            Some(&wipe),
        )
    }
    .ok_or(WifiJoinError::NotPermitted)
}

/// CoreWLAN's `CWErr` codes, as the setup channel's join codes.
fn join_error(code: isize, had_secret: bool) -> WifiJoinError {
    let refused_key = [
        CWErr::CWSupplicantTimeoutErr,
        CWErr::CWChallengeFailureErr,
        CWErr::CWInvalidPMKErr,
        CWErr::CWEAPOLErr,
        CWErr::CWInvalidParameterErr,
    ]
    .iter()
    .any(|err| err.0 == code);
    if code == CWErr::CWOperationNotPermittedErr.0 {
        WifiJoinError::NotPermitted
    } else if refused_key && had_secret {
        // A handshake that fails with a passphrase in hand is the passphrase, in practice.
        WifiJoinError::WrongPassword
    } else {
        WifiJoinError::Timeout
    }
}

fn read_status() -> NetworkStatus {
    let Some(interface) = interface() else {
        return NetworkStatus::disconnected();
    };
    // SAFETY: plain property reads.
    let (name, ssid, associated) = unsafe {
        (
            interface.interfaceName().map(|n| n.to_string()),
            interface.ssid().map(|s| s.to_string()),
            interface.rssiValue() != 0,
        )
    };
    let ipv4: Option<Ipv4Addr> = name.as_deref().and_then(interfaces::ipv4_on);
    let state = match (ipv4, associated) {
        (Some(_), _) => NetworkState::Connected,
        (None, true) => NetworkState::Connecting,
        (None, false) => NetworkState::Disconnected,
    };
    NetworkStatus { state, ssid, ipv4 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_maps_dbm_to_a_percentage() {
        assert_eq!(signal(-120), 0);
        assert_eq!(signal(-100), 0);
        assert_eq!(signal(-75), 50);
        assert_eq!(signal(-50), 100);
        assert_eq!(signal(-20), 100);
    }

    #[test]
    fn corewlan_errors_become_the_specs_codes() {
        let supplicant = CWErr::CWSupplicantTimeoutErr.0;
        assert_eq!(join_error(supplicant, true), WifiJoinError::WrongPassword);
        assert_eq!(join_error(supplicant, false), WifiJoinError::Timeout);
        assert_eq!(
            join_error(CWErr::CWOperationNotPermittedErr.0, true),
            WifiJoinError::NotPermitted
        );
        assert_eq!(
            join_error(CWErr::CWTimeoutErr.0, true),
            WifiJoinError::Timeout
        );
        assert_eq!(
            join_error(CWErr::CWAPFullErr.0, true),
            WifiJoinError::Timeout
        );
    }

    #[test]
    fn the_passphrase_string_reads_our_buffer_and_releases_it() {
        let secret = Zeroizing::new(b"otter-lantern-quietly-91".to_vec());
        let string = passphrase_string(secret).unwrap();
        assert_eq!(string.to_string(), "otter-lantern-quietly-91");
        drop(string);
    }

    /// Reads this Mac's own radio; joins nothing.
    #[tokio::test]
    #[ignore = "reads this Mac's Wi-Fi hardware"]
    async fn this_macs_station_reports_without_faking_names() {
        let station = CoreWlanStation::default();
        let capabilities = station.capabilities().await;
        eprintln!("capabilities: {capabilities:?}");
        eprintln!("status: {:?}", station.status().await);
        match station.scan().await {
            Ok(networks) => assert!(networks.iter().all(|n| !n.ssid.is_empty())),
            Err(error) => eprintln!("scan refused: {error:?}"),
        }
    }
}
