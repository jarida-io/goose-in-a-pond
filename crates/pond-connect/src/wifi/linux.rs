//! The NetworkManager station (`pond-setup-channel.md`, "Per OS"). The passphrase goes from the
//! sealed message to NetworkManager's own root-only store and nowhere else; a join that fails is
//! deleted, so a wrong passphrase is never saved and never autoconnects.

mod nm;
mod zbus_nm;

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use pond_core::network::domain::wifi::{
    JoinedNetwork, NetworkState, NetworkStatus, WifiCapabilities, WifiJoinError, WifiNetwork,
    WifiPassphrase, WifiScanError,
};
use pond_core::network::ports::wifi_station::WifiStation;
use tokio::time::Instant;

use nm::{
    AccessPoint, DeviceLookup, JoinRequest, KeyMgmt, NetworkManager, NmError, StateChange,
    WifiDevice, STATE_ACTIVATED, STATE_DISCONNECTED, STATE_FAILED,
};
use zbus_nm::ZbusNetworkManager;

/// The spec's whole budget for a join, the scan for its network included.
const JOIN_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a scan waits for NetworkManager to finish the one it asked for.
const SCAN_WAIT: Duration = Duration::from_secs(10);
/// How long the radio gets to come up after the pond switched Wi-Fi on.
const RADIO_WAIT: Duration = Duration::from_secs(10);
/// Signals carry every transition; the poll only catches what a lost signal would hide.
const POLL: Duration = Duration::from_millis(500);

pub fn platform() -> Arc<dyn WifiStation> {
    Arc::new(NetworkManagerStation::new(ZbusNetworkManager::new()))
}

struct NetworkManagerStation<N = ZbusNetworkManager> {
    nm: N,
    /// One join at a time; a second one answers `busy` rather than fight over the device.
    joining: tokio::sync::Mutex<()>,
}

/// What a join activated, so a failure can undo exactly that.
enum Attempt {
    /// A new in-memory connection: saved on success, deleted on failure.
    New { connection: String, active: String },
    /// A connection NetworkManager already had: left as it was either way.
    Saved { active: String },
}

impl Attempt {
    fn active(&self) -> &str {
        match self {
            Self::New { active, .. } | Self::Saved { active } => active,
        }
    }
}

impl<N: NetworkManager> NetworkManagerStation<N> {
    fn new(nm: N) -> Self {
        Self {
            nm,
            joining: tokio::sync::Mutex::new(()),
        }
    }

    async fn device(&self) -> Result<WifiDevice, NmError> {
        match self.nm.wifi_device().await? {
            DeviceLookup::Found(device) => Ok(device),
            DeviceLookup::Unmanaged(interface) => Err(NmError::Unreachable(format!(
                "NetworkManager does not manage {interface}, so this pond cannot drive it"
            ))),
            DeviceLookup::Missing => Err(NmError::Unreachable(nm::NO_DEVICE.to_string())),
        }
    }

    /// Switches Wi-Fi on when only the software switch is off: the phone asked for Wi-Fi, so it
    /// is turned on rather than refused.
    async fn radio_on(&self, device: &WifiDevice) -> Result<(), NmError> {
        let (software, hardware) = self.nm.wireless_enabled().await?;
        if !hardware {
            return Err(NmError::Unreachable(nm::HARDWARE_OFF.to_string()));
        }
        if software {
            return Ok(());
        }
        self.nm.enable_wireless().await?;
        tracing::info!(interface = %device.interface, "switched Wi-Fi on for the setup channel");
        let deadline = Instant::now() + RADIO_WAIT;
        while Instant::now() < deadline {
            if self.nm.device_state(&device.path).await?.state >= STATE_DISCONNECTED {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        Ok(())
    }

    /// Asks for a fresh scan and waits for it; a refused or rate-limited one leaves the last.
    async fn fresh_scan(&self, device: &WifiDevice, wait: Duration) -> Result<(), NmError> {
        let before = self.nm.last_scan(&device.path).await.unwrap_or(-1);
        match self.nm.request_scan(&device.path).await {
            Ok(()) => {}
            Err(NmError::Refused(name)) => {
                tracing::debug!(%name, "NetworkManager declined a scan; using its last one");
                return Err(NmError::Refused(name));
            }
            Err(error) => return Err(error),
        }
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
            if self.nm.last_scan(&device.path).await.unwrap_or(before) != before {
                break;
            }
        }
        Ok(())
    }

    async fn known_ssids(&self) -> HashSet<Vec<u8>> {
        match self.nm.saved_connections().await {
            Ok(saved) => saved.into_iter().map(|saved| saved.ssid).collect(),
            Err(error) => {
                tracing::debug!(?error, "saved connections unreadable; none reads as known");
                HashSet::new()
            }
        }
    }

    /// The strongest access point broadcasting `ssid`, scanning once if none is listed yet.
    async fn find(
        &self,
        device: &WifiDevice,
        ssid: &str,
        deadline: Instant,
    ) -> Result<AccessPoint, WifiJoinError> {
        let strongest = |aps: Vec<AccessPoint>| {
            aps.into_iter()
                .filter(|ap| ap.ssid == ssid.as_bytes())
                .max_by_key(|ap| ap.strength)
        };
        let listed = self
            .nm
            .access_points(&device.path)
            .await
            .map_err(|error| nm::join_error(&error))?;
        if let Some(ap) = strongest(listed) {
            return Ok(ap);
        }
        let wait = SCAN_WAIT.min(deadline.saturating_duration_since(Instant::now()));
        let _ = self.fresh_scan(device, wait).await;
        let rescanned = self
            .nm
            .access_points(&device.path)
            .await
            .map_err(|error| nm::join_error(&error))?;
        strongest(rescanned).ok_or(WifiJoinError::NotFound)
    }

    /// Waits for the device to come up on `attempt` with an IPv4 address. A failed transition's
    /// reason is read from its signal: by the time a poll looks, NetworkManager has moved on.
    async fn wait_for_address(
        &self,
        device: &WifiDevice,
        attempt: &Attempt,
        changes: &mut BoxStream<'static, StateChange>,
        deadline: Instant,
    ) -> Result<Ipv4Addr, WifiJoinError> {
        let mut activated = false;
        let mut signals_open = true;
        let mut poll = tokio::time::interval(POLL);
        loop {
            tokio::select! {
                change = changes.next(), if signals_open => match change {
                    Some(change) if change.new == STATE_FAILED => {
                        tracing::info!(reason = change.reason, "NetworkManager failed the join");
                        return Err(nm::join_error_from_reason(change.reason));
                    }
                    Some(_) => {}
                    None => signals_open = false,
                },
                _ = poll.tick() => {
                    let state = self
                        .nm
                        .device_state(&device.path)
                        .await
                        .map_err(|error| nm::join_error(&error))?;
                    if state.state == STATE_FAILED {
                        return Err(nm::join_error_from_reason(state.reason));
                    }
                    if state.state == STATE_ACTIVATED
                        && state.active_connection.as_deref() == Some(attempt.active())
                    {
                        activated = true;
                    }
                    if activated {
                        if let Ok(Some(ipv4)) = self.nm.ipv4(&device.path).await {
                            return Ok(ipv4);
                        }
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    return Err(if activated {
                        WifiJoinError::NoAddress
                    } else {
                        WifiJoinError::Timeout
                    });
                }
            }
        }
    }

    async fn undo(&self, attempt: &Attempt, error: WifiJoinError) {
        if matches!(error, WifiJoinError::Timeout | WifiJoinError::NoAddress) {
            if let Err(error) = self.nm.deactivate(attempt.active()).await {
                tracing::debug!(?error, "could not deactivate the abandoned join");
            }
        }
        if let Attempt::New { connection, .. } = attempt {
            if let Err(error) = self.nm.delete(connection).await {
                tracing::warn!(?error, "could not delete the failed join's connection");
            }
        }
    }

    async fn join_device(
        &self,
        device: &WifiDevice,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
        deadline: Instant,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        let joined = |ipv4| JoinedNetwork {
            ssid: ssid.to_string(),
            ipv4,
        };
        let state = self
            .nm
            .device_state(&device.path)
            .await
            .map_err(|error| nm::join_error(&error))?;
        if nm::is_activating(state.state) {
            return Err(WifiJoinError::Busy);
        }
        if state.state == STATE_ACTIVATED
            && self
                .nm
                .active_ssid(&device.path)
                .await
                .ok()
                .flatten()
                .as_deref()
                == Some(ssid.as_bytes())
        {
            if let Ok(Some(ipv4)) = self.nm.ipv4(&device.path).await {
                tracing::info!(%ssid, "already on the requested Wi-Fi");
                return Ok(joined(ipv4));
            }
        }

        let access_point = if hidden {
            None
        } else {
            Some(self.find(device, ssid, deadline).await?)
        };
        let key_mgmt = match &access_point {
            Some(ap) => nm::key_mgmt(ap)?,
            None => passphrase.map(|_| KeyMgmt::WpaPsk),
        };
        let saved = match passphrase {
            Some(_) => None,
            None => self
                .nm
                .saved_connections()
                .await
                .ok()
                .and_then(|saved| saved.into_iter().find(|c| c.ssid == ssid.as_bytes())),
        };
        if saved.is_none()
            && passphrase.is_none()
            && key_mgmt.is_some_and(KeyMgmt::needs_passphrase)
        {
            return Err(WifiJoinError::WrongPassword);
        }

        let mut changes = self
            .nm
            .state_changes(&device.path)
            .await
            .map_err(|error| nm::join_error(&error))?;
        let ap_path = access_point.as_ref().map(|ap| ap.path.as_str());
        tracing::info!(%ssid, hidden, known = saved.is_some(), "joining Wi-Fi");
        let attempt = match saved {
            Some(saved) => Attempt::Saved {
                active: self
                    .nm
                    .activate(&saved.path, &device.path, ap_path)
                    .await
                    .map_err(|error| nm::join_error(&error))?,
            },
            None => {
                let request = JoinRequest {
                    ssid,
                    key_mgmt,
                    passphrase: passphrase.map(WifiPassphrase::expose),
                    hidden,
                };
                let (connection, active) = self
                    .nm
                    .add_and_activate(&request, &device.path, ap_path)
                    .await
                    .map_err(|error| nm::join_error(&error))?;
                Attempt::New { connection, active }
            }
        };

        match self
            .wait_for_address(device, &attempt, &mut changes, deadline)
            .await
        {
            Ok(ipv4) => {
                if let Attempt::New { connection, .. } = &attempt {
                    if let Err(error) = self.nm.save(connection).await {
                        tracing::warn!(?error, "joined, but NetworkManager did not save it");
                    }
                }
                tracing::info!(%ssid, %ipv4, "joined Wi-Fi");
                Ok(joined(ipv4))
            }
            Err(error) => {
                tracing::info!(%ssid, code = error.code(), "the Wi-Fi join failed");
                self.undo(&attempt, error).await;
                Err(error)
            }
        }
    }
}

#[async_trait]
impl<N: NetworkManager> WifiStation for NetworkManagerStation<N> {
    async fn capabilities(&self) -> WifiCapabilities {
        if let Err(error) = self.device().await {
            return WifiCapabilities::unsupported(&nm::unreachable_reason(&error));
        }
        if let Ok((_, false)) = self.nm.wireless_enabled().await {
            return WifiCapabilities::unsupported(nm::HARDWARE_OFF);
        }
        match self.nm.permissions().await {
            Ok(permissions) => nm::capabilities(&permissions),
            Err(error) => WifiCapabilities::unsupported(&nm::unreachable_reason(&error)),
        }
    }

    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
        let device = self
            .device()
            .await
            .map_err(|error| nm::scan_error(&error))?;
        self.radio_on(&device)
            .await
            .map_err(|error| nm::scan_error(&error))?;
        let declined = self.fresh_scan(&device, SCAN_WAIT).await.err();
        let access_points = self
            .nm
            .access_points(&device.path)
            .await
            .map_err(|error| nm::scan_error(&error))?;
        if access_points.is_empty() {
            if let Some(error) = declined {
                return Err(nm::scan_error(&error));
            }
        }
        Ok(nm::networks(&access_points, &self.known_ssids().await))
    }

    async fn join(
        &self,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        let Ok(_joining) = self.joining.try_lock() else {
            return Err(WifiJoinError::Busy);
        };
        let deadline = Instant::now() + JOIN_TIMEOUT;
        let device = self
            .device()
            .await
            .map_err(|error| nm::join_error(&error))?;
        self.radio_on(&device)
            .await
            .map_err(|error| nm::join_error(&error))?;
        self.join_device(&device, ssid, passphrase, hidden, deadline)
            .await
    }

    async fn status(&self) -> NetworkStatus {
        let Ok(device) = self.device().await else {
            return NetworkStatus::disconnected();
        };
        let Ok(state) = self.nm.device_state(&device.path).await else {
            return NetworkStatus::disconnected();
        };
        let network = if state.state == STATE_ACTIVATED {
            NetworkState::Connected
        } else if nm::is_activating(state.state) {
            NetworkState::Connecting
        } else {
            return NetworkStatus::disconnected();
        };
        let ssid = self
            .nm
            .active_ssid(&device.path)
            .await
            .ok()
            .flatten()
            .and_then(|ssid| String::from_utf8(ssid).ok());
        let ipv4 = match network {
            NetworkState::Connected => self.nm.ipv4(&device.path).await.ok().flatten(),
            _ => None,
        };
        NetworkStatus {
            state: network,
            ssid,
            ipv4,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use pond_core::network::domain::wifi::WifiSecurity;
    use tokio::sync::broadcast;

    use super::nm::{DeviceState, SavedConnection, AP_FLAGS_PRIVACY};
    use super::*;

    const STATE_UNAVAILABLE: u32 = 20;

    const DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/3";
    const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 42);
    const WPA2: u32 = 0x100 | 0x8 | 0x80;
    const PASSPHRASE: &str = "otter-lantern-quietly-91";
    const GRANTED: [&str; 2] = [
        "org.freedesktop.NetworkManager.settings.modify.system",
        "org.freedesktop.NetworkManager.network-control",
    ];

    /// What the fake does once asked to activate: NetworkManager's transitions, in order.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Outcome {
        Joins,
        Fails(u32),
        ActivatesWithoutAddress,
        Hangs,
    }

    #[derive(Default)]
    struct Calls {
        added: Vec<String>,
        activated: Vec<String>,
        saved: Vec<String>,
        deleted: Vec<String>,
        deactivated: Vec<String>,
        scans: u32,
        enabled_wireless: u32,
    }

    struct FakeNm {
        lookup: Mutex<DeviceLookup>,
        state: Mutex<DeviceState>,
        ipv4: Mutex<Option<Ipv4Addr>>,
        active_ssid: Mutex<Option<Vec<u8>>>,
        listed: Mutex<Vec<AccessPoint>>,
        appears_after_scan: Mutex<Vec<AccessPoint>>,
        saved: Mutex<Vec<SavedConnection>>,
        permissions: Mutex<HashMap<String, String>>,
        wireless: Mutex<(bool, bool)>,
        scan_refusal: Mutex<Option<String>>,
        add_refusal: Mutex<Option<String>>,
        outcome: Mutex<Outcome>,
        last_scan: Mutex<i64>,
        signals: broadcast::Sender<StateChange>,
        calls: Mutex<Calls>,
        /// Every passphrase NetworkManager was handed, so a test can prove where it went.
        psks: Mutex<Vec<String>>,
    }

    impl FakeNm {
        fn new() -> Self {
            Self {
                lookup: Mutex::new(DeviceLookup::Found(WifiDevice {
                    path: DEVICE.into(),
                    interface: "wlan0".into(),
                })),
                state: Mutex::new(DeviceState {
                    state: STATE_DISCONNECTED,
                    reason: 0,
                    active_connection: None,
                }),
                ipv4: Mutex::new(None),
                active_ssid: Mutex::new(None),
                listed: Mutex::new(vec![
                    ap("Home", AP_FLAGS_PRIVACY, WPA2, 61),
                    ap("Home", AP_FLAGS_PRIVACY, WPA2, 88),
                    ap("Cafe", 0, 0, 40),
                ]),
                appears_after_scan: Mutex::new(Vec::new()),
                saved: Mutex::new(Vec::new()),
                permissions: Mutex::new(
                    GRANTED
                        .iter()
                        .map(|p| (p.to_string(), "yes".to_string()))
                        .collect(),
                ),
                wireless: Mutex::new((true, true)),
                scan_refusal: Mutex::new(None),
                add_refusal: Mutex::new(None),
                outcome: Mutex::new(Outcome::Joins),
                last_scan: Mutex::new(1_000),
                signals: broadcast::channel(64).0,
                calls: Mutex::new(Calls::default()),
                psks: Mutex::new(Vec::new()),
            }
        }

        fn set<T>(cell: &Mutex<T>, value: T) {
            *cell.lock().unwrap() = value;
        }

        fn calls<R>(&self, read: impl FnOnce(&Calls) -> R) -> R {
            read(&self.calls.lock().unwrap())
        }

        /// Plays NetworkManager's transitions for an activation, as signals and as state.
        fn play(self: &Arc<Self>, active: String, ssid: Vec<u8>) {
            let outcome = *self.outcome.lock().unwrap();
            let nm = self.clone();
            tokio::spawn(async move {
                for state in [40, 50, 60] {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    nm.transition(state, 0, Some(&active));
                }
                match outcome {
                    Outcome::Hangs => {}
                    Outcome::Fails(reason) => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        nm.transition(STATE_FAILED, reason, None);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        nm.transition(STATE_DISCONNECTED, 0, None);
                    }
                    Outcome::Joins | Outcome::ActivatesWithoutAddress => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        nm.transition(70, 0, Some(&active));
                        *nm.active_ssid.lock().unwrap() = Some(ssid);
                        if outcome == Outcome::Joins {
                            *nm.ipv4.lock().unwrap() = Some(ADDRESS);
                        }
                        nm.transition(STATE_ACTIVATED, 0, Some(&active));
                    }
                }
            });
        }

        fn transition(&self, state: u32, reason: u32, active: Option<&str>) {
            *self.state.lock().unwrap() = DeviceState {
                state,
                reason,
                active_connection: active.map(str::to_string),
            };
            let _ = self.signals.send(StateChange { new: state, reason });
        }
    }

    fn ap(ssid: &str, flags: u32, rsn: u32, strength: u8) -> AccessPoint {
        AccessPoint {
            path: format!("/org/freedesktop/NetworkManager/AccessPoint/{ssid}{strength}"),
            ssid: ssid.as_bytes().to_vec(),
            flags,
            wpa_flags: 0,
            rsn_flags: rsn,
            strength,
        }
    }

    fn refused(name: &Mutex<Option<String>>) -> Result<(), NmError> {
        match name.lock().unwrap().clone() {
            Some(name) => Err(NmError::Refused(name)),
            None => Ok(()),
        }
    }

    #[async_trait]
    impl NetworkManager for Arc<FakeNm> {
        async fn wifi_device(&self) -> Result<DeviceLookup, NmError> {
            Ok(self.lookup.lock().unwrap().clone())
        }

        async fn permissions(&self) -> Result<HashMap<String, String>, NmError> {
            Ok(self.permissions.lock().unwrap().clone())
        }

        async fn wireless_enabled(&self) -> Result<(bool, bool), NmError> {
            Ok(*self.wireless.lock().unwrap())
        }

        async fn enable_wireless(&self) -> Result<(), NmError> {
            self.calls.lock().unwrap().enabled_wireless += 1;
            self.wireless.lock().unwrap().0 = true;
            Ok(())
        }

        async fn request_scan(&self, _device: &str) -> Result<(), NmError> {
            self.calls.lock().unwrap().scans += 1;
            refused(&self.scan_refusal)?;
            let appeared = std::mem::take(&mut *self.appears_after_scan.lock().unwrap());
            self.listed.lock().unwrap().extend(appeared);
            *self.last_scan.lock().unwrap() += 1;
            Ok(())
        }

        async fn last_scan(&self, _device: &str) -> Result<i64, NmError> {
            Ok(*self.last_scan.lock().unwrap())
        }

        async fn access_points(&self, _device: &str) -> Result<Vec<AccessPoint>, NmError> {
            Ok(self.listed.lock().unwrap().clone())
        }

        async fn saved_connections(&self) -> Result<Vec<SavedConnection>, NmError> {
            Ok(self.saved.lock().unwrap().clone())
        }

        async fn device_state(&self, _device: &str) -> Result<DeviceState, NmError> {
            Ok(self.state.lock().unwrap().clone())
        }

        async fn active_ssid(&self, _device: &str) -> Result<Option<Vec<u8>>, NmError> {
            Ok(self.active_ssid.lock().unwrap().clone())
        }

        async fn ipv4(&self, _device: &str) -> Result<Option<Ipv4Addr>, NmError> {
            Ok(*self.ipv4.lock().unwrap())
        }

        async fn state_changes(
            &self,
            _device: &str,
        ) -> Result<BoxStream<'static, StateChange>, NmError> {
            let signals = self.signals.subscribe();
            Ok(futures::stream::unfold(signals, |mut signals| async move {
                loop {
                    match signals.recv().await {
                        Ok(change) => return Some((change, signals)),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            })
            .boxed())
        }

        async fn add_and_activate(
            &self,
            request: &JoinRequest<'_>,
            _device: &str,
            _access_point: Option<&str>,
        ) -> Result<(String, String), NmError> {
            refused(&self.add_refusal)?;
            if let Some(psk) = request.passphrase {
                self.psks.lock().unwrap().push(psk.to_string());
            }
            let connection = format!("/Settings/{}", request.ssid);
            let active = format!("/ActiveConnection/{}", request.ssid);
            self.calls
                .lock()
                .unwrap()
                .added
                .push(format!("{request:?}"));
            self.play(active.clone(), request.ssid.as_bytes().to_vec());
            Ok((connection, active))
        }

        async fn activate(
            &self,
            connection: &str,
            _device: &str,
            _access_point: Option<&str>,
        ) -> Result<String, NmError> {
            self.calls
                .lock()
                .unwrap()
                .activated
                .push(connection.to_string());
            let ssid = self
                .saved
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.path == connection)
                .map(|c| c.ssid.clone())
                .unwrap_or_default();
            let active = format!("/ActiveConnection/saved{connection}");
            self.play(active.clone(), ssid);
            Ok(active)
        }

        async fn deactivate(&self, active: &str) -> Result<(), NmError> {
            self.calls
                .lock()
                .unwrap()
                .deactivated
                .push(active.to_string());
            Ok(())
        }

        async fn save(&self, connection: &str) -> Result<(), NmError> {
            self.calls
                .lock()
                .unwrap()
                .saved
                .push(connection.to_string());
            Ok(())
        }

        async fn delete(&self, connection: &str) -> Result<(), NmError> {
            self.calls
                .lock()
                .unwrap()
                .deleted
                .push(connection.to_string());
            Ok(())
        }
    }

    fn new_station() -> (NetworkManagerStation<Arc<FakeNm>>, Arc<FakeNm>) {
        let nm = Arc::new(FakeNm::new());
        (NetworkManagerStation::new(nm.clone()), nm)
    }

    fn passphrase() -> WifiPassphrase {
        WifiPassphrase::new(PASSPHRASE.into()).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_wpa2_join_hands_the_passphrase_to_nm_and_saves_only_once_it_works() {
        let (station, nm) = new_station();
        let joined = station
            .join("Home", Some(&passphrase()), false)
            .await
            .unwrap();
        assert_eq!(
            joined,
            JoinedNetwork {
                ssid: "Home".into(),
                ipv4: ADDRESS
            }
        );
        assert_eq!(*nm.psks.lock().unwrap(), vec![PASSPHRASE.to_string()]);
        nm.calls(|calls| {
            assert_eq!(calls.added.len(), 1);
            assert!(calls.added[0].contains("WpaPsk") && !calls.added[0].contains(PASSPHRASE));
            assert_eq!(calls.saved, vec!["/Settings/Home".to_string()]);
            assert!(calls.deleted.is_empty());
        });
    }

    #[tokio::test(start_paused = true)]
    async fn a_wrong_passphrase_is_wrong_password_and_its_connection_is_deleted_unsaved() {
        let (station, nm) = new_station();
        FakeNm::set(&nm.outcome, Outcome::Fails(7));
        assert_eq!(
            station.join("Home", Some(&passphrase()), false).await,
            Err(WifiJoinError::WrongPassword)
        );
        nm.calls(|calls| {
            assert!(
                calls.saved.is_empty(),
                "a wrong passphrase never reaches disk"
            );
            assert_eq!(calls.deleted, vec!["/Settings/Home".to_string()]);
        });
    }

    #[tokio::test(start_paused = true)]
    async fn each_failed_activation_reads_as_its_code() {
        for (reason, code) in [
            (53, WifiJoinError::NotFound),
            (5, WifiJoinError::NoAddress),
            (10, WifiJoinError::WrongPassword),
            (60, WifiJoinError::Busy),
        ] {
            let (station, nm) = new_station();
            FakeNm::set(&nm.outcome, Outcome::Fails(reason));
            assert_eq!(
                station.join("Home", Some(&passphrase()), false).await,
                Err(code),
                "reason {reason}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_activation_that_never_finishes_times_out_inside_the_budget_and_is_undone() {
        let (station, nm) = new_station();
        FakeNm::set(&nm.outcome, Outcome::Hangs);
        let started = Instant::now();
        assert_eq!(
            station.join("Home", Some(&passphrase()), false).await,
            Err(WifiJoinError::Timeout)
        );
        assert!(started.elapsed() <= JOIN_TIMEOUT + POLL);
        nm.calls(|calls| {
            assert_eq!(
                calls.deactivated,
                vec!["/ActiveConnection/Home".to_string()]
            );
            assert_eq!(calls.deleted, vec!["/Settings/Home".to_string()]);
        });
    }

    #[tokio::test(start_paused = true)]
    async fn activated_without_an_ipv4_address_is_no_address() {
        let (station, nm) = new_station();
        FakeNm::set(&nm.outcome, Outcome::ActivatesWithoutAddress);
        assert_eq!(
            station.join("Home", Some(&passphrase()), false).await,
            Err(WifiJoinError::NoAddress)
        );
        nm.calls(|calls| assert_eq!(calls.deleted.len(), 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_network_out_of_range_is_not_found_after_one_scan_and_nothing_is_added() {
        let (station, nm) = new_station();
        assert_eq!(
            station.join("Attic", Some(&passphrase()), false).await,
            Err(WifiJoinError::NotFound)
        );
        nm.calls(|calls| {
            assert_eq!(calls.scans, 1);
            assert!(calls.added.is_empty());
        });
    }

    #[tokio::test(start_paused = true)]
    async fn a_network_that_shows_up_on_the_rescan_is_joined() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.appears_after_scan,
            vec![ap("Attic", AP_FLAGS_PRIVACY, WPA2, 30)],
        );
        assert!(station
            .join("Attic", Some(&passphrase()), false)
            .await
            .is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_hidden_network_is_asked_for_by_name_without_a_scan_match() {
        let (station, nm) = new_station();
        assert!(station
            .join("Vault", Some(&passphrase()), true)
            .await
            .is_ok());
        nm.calls(|calls| {
            assert!(calls.added[0].contains("hidden: true"));
            assert_eq!(calls.scans, 0);
        });
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_network_joins_with_no_passphrase_and_a_secured_unknown_one_needs_one() {
        let (station, nm) = new_station();
        assert!(station.join("Cafe", None, false).await.is_ok());
        nm.calls(|calls| assert!(calls.added[0].contains("key_mgmt: None")));

        let (station, nm) = new_station();
        assert_eq!(
            station.join("Home", None, false).await,
            Err(WifiJoinError::WrongPassword)
        );
        nm.calls(|calls| assert!(calls.added.is_empty()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_known_network_without_a_passphrase_reuses_the_saved_connection() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.saved,
            vec![SavedConnection {
                path: "/Settings/7".into(),
                ssid: b"Home".to_vec(),
            }],
        );
        assert!(station.join("Home", None, false).await.is_ok());
        nm.calls(|calls| {
            assert_eq!(calls.activated, vec!["/Settings/7".to_string()]);
            assert!(calls.added.is_empty() && calls.saved.is_empty());
        });
    }

    #[tokio::test(start_paused = true)]
    async fn joining_the_network_it_is_already_on_answers_without_touching_it() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.state,
            DeviceState {
                state: STATE_ACTIVATED,
                reason: 0,
                active_connection: Some("/ActiveConnection/1".into()),
            },
        );
        FakeNm::set(&nm.active_ssid, Some(b"Home".to_vec()));
        FakeNm::set(&nm.ipv4, Some(ADDRESS));
        assert_eq!(
            station
                .join("Home", Some(&passphrase()), false)
                .await
                .unwrap()
                .ipv4,
            ADDRESS
        );
        nm.calls(|calls| assert!(calls.added.is_empty() && calls.activated.is_empty()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_mid_activation_or_a_second_join_is_busy() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.state,
            DeviceState {
                state: 50,
                reason: 0,
                active_connection: Some("/ActiveConnection/1".into()),
            },
        );
        assert_eq!(
            station.join("Home", Some(&passphrase()), false).await,
            Err(WifiJoinError::Busy)
        );

        let (station, nm) = new_station();
        FakeNm::set(&nm.outcome, Outcome::Hangs);
        let held = passphrase();
        let first = station.join("Home", Some(&held), false);
        let second = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            station.join("Cafe", None, false).await
        };
        let (_, second) = tokio::join!(first, second);
        assert_eq!(second, Err(WifiJoinError::Busy));
    }

    #[tokio::test(start_paused = true)]
    async fn polkit_refusing_the_join_is_not_permitted() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.add_refusal,
            Some("org.freedesktop.NetworkManager.Settings.PermissionDenied".into()),
        );
        assert_eq!(
            station.join("Home", Some(&passphrase()), false).await,
            Err(WifiJoinError::NotPermitted)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_wifi_adapter_is_unsupported_everywhere_and_says_why() {
        let (station, nm) = new_station();
        FakeNm::set(&nm.lookup, DeviceLookup::Missing);
        assert_eq!(
            station.join("Home", Some(&passphrase()), false).await,
            Err(WifiJoinError::Unsupported)
        );
        assert_eq!(station.scan().await, Err(WifiScanError::Unsupported));
        assert_eq!(
            station.capabilities().await.join.reason(),
            Some(nm::NO_DEVICE)
        );
        assert_eq!(station.status().await, NetworkStatus::disconnected());

        FakeNm::set(&nm.lookup, DeviceLookup::Unmanaged("wlan0".into()));
        assert!(station
            .capabilities()
            .await
            .scan
            .reason()
            .is_some_and(|reason| reason.contains("does not manage wlan0")));
    }

    #[tokio::test(start_paused = true)]
    async fn capabilities_say_the_grant_is_missing_before_a_join_tries() {
        let (station, nm) = new_station();
        assert_eq!(station.capabilities().await, WifiCapabilities::available());
        FakeNm::set(&nm.permissions, HashMap::new());
        let capabilities = station.capabilities().await;
        assert!(capabilities.scan.is_available());
        assert_eq!(capabilities.join.reason(), Some(nm::NO_GRANT));
        FakeNm::set(&nm.wireless, (true, false));
        assert_eq!(
            station.capabilities().await.join.reason(),
            Some(nm::HARDWARE_OFF)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_scan_lists_what_nm_sees_with_saved_networks_marked_known() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.saved,
            vec![SavedConnection {
                path: "/Settings/7".into(),
                ssid: b"Home".to_vec(),
            }],
        );
        let networks = station.scan().await.unwrap();
        assert_eq!(networks.len(), 3, "the station reports, the session ranks");
        assert!(networks.iter().all(|n| (n.ssid == "Home") == n.known));
        assert!(networks
            .iter()
            .any(|n| n.ssid == "Cafe" && n.security == WifiSecurity::Open));
        nm.calls(|calls| assert_eq!(calls.scans, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_declined_scan_still_lists_nms_last_one_and_is_refused_only_when_empty() {
        let (station, nm) = new_station();
        FakeNm::set(
            &nm.scan_refusal,
            Some("org.freedesktop.NetworkManager.PermissionDenied".into()),
        );
        assert_eq!(station.scan().await.unwrap().len(), 3);
        FakeNm::set(&nm.listed, Vec::new());
        assert_eq!(station.scan().await, Err(WifiScanError::NotPermitted));
    }

    #[tokio::test(start_paused = true)]
    async fn wifi_switched_off_in_software_is_switched_on_for_the_phone() {
        let (station, nm) = new_station();
        FakeNm::set(&nm.wireless, (false, true));
        FakeNm::set(
            &nm.state,
            DeviceState {
                state: STATE_UNAVAILABLE,
                reason: 0,
                active_connection: None,
            },
        );
        let nm_for_radio = nm.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            nm_for_radio.transition(STATE_DISCONNECTED, 0, None);
        });
        assert!(station.scan().await.is_ok());
        nm.calls(|calls| assert_eq!(calls.enabled_wireless, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn status_reports_the_link_the_device_has() {
        let (station, nm) = new_station();
        assert_eq!(station.status().await, NetworkStatus::disconnected());
        FakeNm::set(
            &nm.state,
            DeviceState {
                state: STATE_ACTIVATED,
                reason: 0,
                active_connection: Some("/ActiveConnection/1".into()),
            },
        );
        FakeNm::set(&nm.active_ssid, Some(b"Home".to_vec()));
        FakeNm::set(&nm.ipv4, Some(ADDRESS));
        assert_eq!(
            station.status().await,
            NetworkStatus {
                state: NetworkState::Connected,
                ssid: Some("Home".into()),
                ipv4: Some(ADDRESS),
            }
        );
        FakeNm::set(
            &nm.state,
            DeviceState {
                state: 60,
                reason: 0,
                active_connection: Some("/ActiveConnection/2".into()),
            },
        );
        assert_eq!(station.status().await.state, NetworkState::Connecting);
        assert_eq!(station.status().await.ipv4, None);
    }
}
