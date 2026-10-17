//! The BlueZ peripheral (`pond-setup-channel.md`, "Per OS"): bluer serves the setup service and
//! the advertisement over D-Bus, and [`router`] gives each central's writes to one session.
//!
//! No agent, no pairable or discoverable change: the advertisement's own `Discoverable` writes
//! the LE flags, and no characteristic asks for encryption, so a phone never sees a pairing prompt.

mod rfkill;
mod router;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bluer::adv::{
    Advertisement as LeAdvertisement, AdvertisementHandle, Type as AdvertisementType,
};
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicNotifier, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicWrite, CharacteristicWriteMethod, ReqError, Service,
};
use bluer::{Adapter, Address, DeviceEvent, DeviceProperty, ErrorKind, InternalErrorKind, Uuid};
use futures::future::{BoxFuture, Shared};
use futures::{FutureExt, StreamExt};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use super::peripheral::{Advertisement, BleError, BlePeripheral, PeripheralStatus};
use super::{RX_CHARACTERISTIC_UUID, SERVICE_UUID, TX_CHARACTERISTIC_UUID};
use crate::setup_channel::SetupChannel;
use router::{Notifier, NotifierStopped, Radio, Refusal, Router};

/// How often the peripheral checks that bluetoothd still holds its registration.
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
/// A scan response holds 31 bytes and a Complete Local Name spends 2 on its header; BlueZ shortens
/// a longer name to a Shortened Local Name rather than refuse it.
const MAX_LOCAL_NAME_BYTES: usize = 29;
const BLUEZ_BUS_NAME: &str = "org.bluez";
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";

const NOT_STARTED: &str = "the BLE peripheral has not been started";
const BLUETOOTHD_DOWN: &str = "bluetoothd is not running (sudo systemctl start bluetooth)";
const NO_ADAPTER: &str = "this host has no Bluetooth adapter";
const POWERED_OFF: &str = "the Bluetooth adapter is powered off";
const NO_LE_PERIPHERAL: &str = "this Bluetooth adapter cannot advertise over LE";

pub fn platform() -> Arc<dyn BlePeripheral> {
    Arc::new(BluezPeripheral::new())
}

/// `start` succeeds even when the radio is not up yet: the advertising controller starts a
/// peripheral only once, and this one keeps trying every [`HEALTH_INTERVAL`] (bluetoothd late at
/// boot, an rfkill lifted, a restart), so `status` and `advertise` say why until it heals.
pub struct BluezPeripheral {
    shared: Arc<SharedState>,
}

struct SharedState {
    status: Mutex<PeripheralStatus>,
    control: AsyncMutex<Control>,
}

#[derive(Default)]
struct Control {
    channel: Option<Arc<SetupChannel>>,
    /// What the server last asked for; re-applied when the radio comes back, because the server
    /// only asks again when its own decision changes.
    wanted: Option<Advertisement>,
    live: Option<Live>,
    reason: Option<String>,
    bus: Option<zbus::Connection>,
    /// The adapter is powered on at most once per process, so a deliberate power-off (Matter's
    /// raw-HCI commissioning takes the adapter down) is never fought.
    power_attempted: bool,
    supervisor: Option<JoinHandle<()>>,
}

/// Everything registered with one bluetoothd; dropping it unregisters both.
struct Live {
    adapter: Adapter,
    owner: String,
    router: Arc<Router<BluezRadio>>,
    advertising: Option<(Advertisement, AdvertisementHandle)>,
    _app: ApplicationHandle,
    _session: bluer::Session,
}

impl BluezPeripheral {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(SharedState {
                status: Mutex::new(PeripheralStatus::Off),
                control: AsyncMutex::new(Control::default()),
            }),
        }
    }
}

impl Default for BluezPeripheral {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BlePeripheral for BluezPeripheral {
    async fn start(&self, channel: Arc<SetupChannel>) -> Result<(), BleError> {
        let mut control = self.shared.control.lock().await;
        if control.channel.is_none() {
            control.channel = Some(channel);
            if let Err(reason) = bring_up(&mut control).await {
                control.reason = Some(reason);
            }
            control.supervisor = Some(supervise(Arc::downgrade(&self.shared)));
        }
        self.shared.publish(&control);
        Ok(())
    }

    async fn advertise(&self, advertisement: Option<Advertisement>) -> Result<(), BleError> {
        if let Some(advertisement) = &advertisement {
            check_local_name(&advertisement.local_name)?;
        }
        let mut control = self.shared.control.lock().await;
        control.wanted = advertisement;
        let reason = unavailable_reason(&control);
        let Control { live, wanted, .. } = &mut *control;
        let result = match live {
            Some(live) => sync_advertising(live, wanted.as_ref()).await,
            None if wanted.is_none() => Ok(()),
            None => Err(BleError::Unavailable(reason)),
        };
        self.shared.publish(&control);
        result
    }

    async fn shutdown(&self) {
        let mut control = self.shared.control.lock().await;
        if let Some(supervisor) = control.supervisor.take() {
            supervisor.abort();
        }
        if let Some(live) = control.live.take() {
            live.router.shutdown();
        }
        control.channel = None;
        control.wanted = None;
        control.reason = None;
        self.shared.publish(&control);
    }

    fn status(&self) -> PeripheralStatus {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl SharedState {
    /// One health check: a registration that bluetoothd lost is dropped, and a missing one is
    /// retried, so a restarted bluetoothd or a re-enabled adapter heals without a pond restart.
    async fn tick(&self) {
        let mut control = self.control.lock().await;
        if control.channel.is_none() {
            return;
        }
        if let Some(live) = &control.live {
            let bus = control.bus.clone();
            match still_registered(bus.as_ref(), live).await {
                Ok(()) => return,
                Err(reason) => {
                    if let Some(live) = control.live.take() {
                        live.router.shutdown();
                    }
                    control.reason = Some(reason);
                    self.publish(&control);
                }
            }
        }
        match bring_up(&mut control).await {
            Ok(()) => {
                let Control { live, wanted, .. } = &mut *control;
                if let Some(live) = live {
                    if let Err(error) = sync_advertising(live, wanted.as_ref()).await {
                        tracing::warn!(%error, "could not restore the setup channel's advertisement");
                    }
                }
            }
            Err(reason) => control.reason = Some(reason),
        }
        self.publish(&control);
    }

    fn publish(&self, control: &Control) {
        let next = match (&control.channel, &control.live) {
            (None, _) => PeripheralStatus::Off,
            (Some(_), Some(live)) if live.advertising.is_some() => PeripheralStatus::Advertising,
            (Some(_), Some(_)) => PeripheralStatus::Serving,
            (Some(_), None) => PeripheralStatus::Unavailable {
                reason: unavailable_reason(control),
            },
        };
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == next {
            return;
        }
        match &next {
            PeripheralStatus::Unavailable { reason } => {
                tracing::warn!(kind = "ble_unavailable", %reason, "the BLE setup channel is unavailable");
            }
            PeripheralStatus::Serving => {
                tracing::info!(kind = "ble_serving", "the BLE setup channel is serving");
            }
            PeripheralStatus::Advertising => {
                tracing::info!(
                    kind = "ble_advertising",
                    "the BLE setup channel is advertising"
                );
            }
            PeripheralStatus::Off => {
                tracing::info!(kind = "ble_off", "the BLE setup channel is off")
            }
        }
        *status = next;
    }
}

fn unavailable_reason(control: &Control) -> String {
    control
        .reason
        .clone()
        .unwrap_or_else(|| NOT_STARTED.to_string())
}

fn supervise(shared: Weak<SharedState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(HEALTH_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await;
        loop {
            ticks.tick().await;
            let Some(shared) = shared.upgrade() else {
                return;
            };
            shared.tick().await;
        }
    })
}

/// Registers the setup service with bluetoothd's default adapter. `Err` is the reason the
/// household reads.
async fn bring_up(control: &mut Control) -> Result<(), String> {
    let Some(channel) = control.channel.clone() else {
        return Err(NOT_STARTED.to_string());
    };
    if let Some(block) = rfkill::blocked(Path::new(rfkill::SYSFS_RFKILL), "bluetooth") {
        return Err(block.reason("bluetooth"));
    }
    let bus = system_bus(control).await?;
    let owner = bluez_owner(&bus).await?;
    let session = bluer::Session::new()
        .await
        .map_err(|error| format!("cannot reach BlueZ over D-Bus: {error}"))?;
    let adapter = session
        .default_adapter()
        .await
        .map_err(|error| bluez_reason(&error))?;
    let powered = adapter
        .is_powered()
        .await
        .map_err(|error| bluez_reason(&error))?;
    let first_look = !control.power_attempted;
    control.power_attempted = true;
    if !powered {
        if !first_look {
            return Err(POWERED_OFF.to_string());
        }
        adapter
            .set_powered(true)
            .await
            .map_err(|error| format!("{POWERED_OFF}, and powering it on failed: {error}"))?;
        tracing::info!(
            adapter = adapter.name(),
            "powered the Bluetooth adapter on for the setup channel"
        );
    }
    match adapter.supported_advertising_instances().await {
        Ok(instances) if instances > 0 => {}
        _ => return Err(NO_LE_PERIPHERAL.to_string()),
    }
    let router = Router::new(
        channel,
        Arc::new(BluezRadio {
            adapter: adapter.clone(),
        }),
    );
    let app = adapter
        .serve_gatt_application(application(router.clone()))
        .await
        .map_err(|error| format!("BlueZ refused the setup service: {error}"))?;
    tracing::info!(
        adapter = adapter.name(),
        "registered the setup channel's GATT service"
    );
    control.reason = None;
    control.live = Some(Live {
        adapter,
        owner,
        router,
        advertising: None,
        _app: app,
        _session: session,
    });
    Ok(())
}

/// `Err` when bluetoothd restarted (it forgets every registration) or the adapter went down.
async fn still_registered(bus: Option<&zbus::Connection>, live: &Live) -> Result<(), String> {
    let Some(bus) = bus else {
        return Err("lost the D-Bus system bus".to_string());
    };
    let owner = bluez_owner(bus).await?;
    if owner != live.owner {
        return Err("bluetoothd restarted; registering the setup service again".to_string());
    }
    match live.adapter.is_powered().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(
            match rfkill::blocked(Path::new(rfkill::SYSFS_RFKILL), "bluetooth") {
                Some(block) => block.reason("bluetooth"),
                None => POWERED_OFF.to_string(),
            },
        ),
        Err(error) => Err(bluez_reason(&error)),
    }
}

async fn system_bus(control: &mut Control) -> Result<zbus::Connection, String> {
    if let Some(bus) = &control.bus {
        return Ok(bus.clone());
    }
    let bus = zbus::Connection::system()
        .await
        .map_err(|error| format!("cannot reach the D-Bus system bus: {error}"))?;
    control.bus = Some(bus.clone());
    Ok(bus)
}

/// bluetoothd's unique bus name: it changes when bluetoothd restarts.
async fn bluez_owner(bus: &zbus::Connection) -> Result<String, String> {
    let dbus = zbus::fdo::DBusProxy::new(bus)
        .await
        .map_err(|error| format!("cannot ask D-Bus about bluetoothd: {error}"))?;
    let name = zbus::names::BusName::try_from(BLUEZ_BUS_NAME)
        .map_err(|error| format!("cannot ask D-Bus about bluetoothd: {error}"))?;
    match dbus.get_name_owner(name).await {
        Ok(owner) => Ok(owner.to_string()),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => Err(BLUETOOTHD_DOWN.to_string()),
        Err(error) => Err(format!("cannot ask D-Bus about bluetoothd: {error}")),
    }
}

fn bluez_reason(error: &bluer::Error) -> String {
    match &error.kind {
        ErrorKind::NotFound => NO_ADAPTER.to_string(),
        ErrorKind::Internal(InternalErrorKind::DBus(name)) if name == SERVICE_UNKNOWN => {
            BLUETOOTHD_DOWN.to_string()
        }
        _ => format!("BlueZ failed: {error}"),
    }
}

fn check_local_name(name: &str) -> Result<(), BleError> {
    if name.is_empty() || name.len() > MAX_LOCAL_NAME_BYTES {
        return Err(BleError::Failed(format!(
            "a local name of {} bytes does not fit a scan response whole (1 to {MAX_LOCAL_NAME_BYTES})",
            name.len()
        )));
    }
    Ok(())
}

/// The spec's advertisement and nothing else. `LocalName`, never `Includes: ["local-name"]`: BlueZ
/// 5.64 puts `LocalName` in the scan response only, where the include would put the adapter's own
/// name. `Discoverable` writes the LE flags into this advertisement without making the adapter
/// discoverable.
fn le_advertisement(advertisement: &Advertisement) -> LeAdvertisement {
    LeAdvertisement {
        advertisement_type: AdvertisementType::Peripheral,
        service_uuids: BTreeSet::from([Uuid::from_u128(SERVICE_UUID)]),
        discoverable: Some(true),
        local_name: Some(advertisement.local_name.clone()),
        ..Default::default()
    }
}

/// Re-registers only when what is wanted changed, so the server's periodic call costs nothing.
async fn sync_advertising(live: &mut Live, wanted: Option<&Advertisement>) -> Result<(), BleError> {
    let Some(wanted) = wanted else {
        live.advertising = None;
        return Ok(());
    };
    if live
        .advertising
        .as_ref()
        .is_some_and(|(current, _)| current == wanted)
    {
        return Ok(());
    }
    live.advertising = None;
    let handle = live
        .adapter
        .advertise(le_advertisement(wanted))
        .await
        .map_err(|error| BleError::Failed(format!("BlueZ refused the advertisement: {error}")))?;
    tracing::info!(local_name = %wanted.local_name, "advertising the setup channel");
    live.advertising = Some((wanted.clone(), handle));
    Ok(())
}

fn application(router: Arc<Router<BluezRadio>>) -> Application {
    let writes = router.clone();
    let subscriptions = router;
    Application {
        services: vec![Service {
            uuid: Uuid::from_u128(SERVICE_UUID),
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: Uuid::from_u128(RX_CHARACTERISTIC_UUID),
                    write: Some(CharacteristicWrite {
                        write: true,
                        method: CharacteristicWriteMethod::Fun(Box::new(move |value, request| {
                            let router = writes.clone();
                            async move {
                                // Every chunk fits one write, so a long (offset) write is a client bug.
                                if request.offset != 0 {
                                    return Err(ReqError::InvalidOffset);
                                }
                                router
                                    .on_write(request.device_address, request.mtu, &value)
                                    .await
                                    .map_err(att_error)
                            }
                            .boxed()
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: Uuid::from_u128(TX_CHARACTERISTIC_UUID),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                            subscriptions.on_subscribe(Arc::new(BluezNotifier::new(notifier)));
                            async {}.boxed()
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Write Not Permitted (0x03) for a second central: CoreBluetooth can answer it too, so both OSes
/// refuse alike; never an authorization error, which would make the phone's OS start pairing.
fn att_error(refusal: Refusal) -> ReqError {
    match refusal {
        Refusal::Busy => ReqError::NotPermitted,
        Refusal::Ended => ReqError::Failed,
    }
}

/// Notifications go out as `Value` changes, which BlueZ caps at ATT_MTU - 3 per device: the same
/// limit the session chunks to. bluer's socket path would cap them two bytes lower, below what an
/// ATT_MTU of 23 needs.
struct BluezNotifier {
    notifier: AsyncMutex<CharacteristicNotifier>,
    stopped: Shared<BoxFuture<'static, ()>>,
}

impl BluezNotifier {
    fn new(notifier: CharacteristicNotifier) -> Self {
        Self {
            stopped: notifier.stopped().boxed().shared(),
            notifier: AsyncMutex::new(notifier),
        }
    }
}

#[async_trait]
impl Notifier for BluezNotifier {
    async fn notify(&self, chunk: &[u8]) -> Result<(), NotifierStopped> {
        self.notifier
            .lock()
            .await
            .notify(chunk.to_vec())
            .await
            .map_err(|_| NotifierStopped)
    }

    async fn stopped(&self) {
        self.stopped.clone().await;
    }
}

struct BluezRadio {
    adapter: Adapter,
}

#[async_trait]
impl Radio for BluezRadio {
    type Central = Address;

    async fn link_lost(&self, address: Address) {
        let Ok(device) = self.adapter.device(address) else {
            return;
        };
        // Subscribed before the first read, so a drop in between is not missed.
        let events = match device.events().await {
            Ok(events) => events,
            Err(error) => {
                tracing::debug!(%error, "cannot watch the phone's link; treating it as gone");
                return;
            }
        };
        if !device.is_connected().await.unwrap_or(false) {
            return;
        }
        futures::pin_mut!(events);
        while let Some(DeviceEvent::PropertyChanged(property)) = events.next().await {
            if matches!(property, DeviceProperty::Connected(false)) {
                return;
            }
        }
    }

    async fn disconnect(&self, address: Address) {
        let result = match self.adapter.device(address) {
            Ok(device) => device.disconnect().await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            tracing::debug!(%error, "could not drop the phone's link after its session ended");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::local_name;
    use pond_core::network::domain::discriminator::Discriminator;

    /// Flags (3) and a complete list of one 128-bit UUID (18); a legacy PDU carries 31.
    const ADV_DATA_LEN: usize = 3 + 18;
    const LEGACY_PDU_LEN: usize = 31;

    #[test]
    fn the_advertisement_is_the_uuid_and_the_name_and_nothing_else() {
        let advertisement = Advertisement {
            local_name: local_name(Discriminator::new(0xc1cd)),
        };
        let le = le_advertisement(&advertisement);
        assert_eq!(le.advertisement_type, AdvertisementType::Peripheral);
        assert_eq!(
            le.service_uuids,
            BTreeSet::from([Uuid::from_u128(SERVICE_UUID)])
        );
        assert_eq!(le.local_name.as_deref(), Some("Pond c1cd"));
        assert_eq!(le.discoverable, Some(true));
        assert!(
            le.system_includes.is_empty(),
            "an include would add the adapter's own name"
        );
        assert!(le.manufacturer_data.is_empty() && le.service_data.is_empty());
        assert!(le.solicit_uuids.is_empty() && le.advertising_data.is_empty());
        assert_eq!((le.appearance, le.tx_power), (None, None));
    }

    #[test]
    fn the_name_cannot_share_the_pdu_with_the_uuid_but_fits_the_scan_response_whole() {
        let name = local_name(Discriminator::new(0xc1cd));
        assert!(
            ADV_DATA_LEN + 2 + name.len() > LEGACY_PDU_LEN,
            "if this ever fits, BlueZ still sends it in the scan response"
        );
        assert!(2 + name.len() <= LEGACY_PDU_LEN);
        assert!(check_local_name(&name).is_ok());
        assert!(check_local_name(&"x".repeat(MAX_LOCAL_NAME_BYTES)).is_ok());
        assert!(
            check_local_name(&"x".repeat(MAX_LOCAL_NAME_BYTES + 1)).is_err(),
            "BlueZ would shorten it, and a phone would no longer find the card's id"
        );
        assert!(check_local_name("").is_err());
    }

    #[test]
    fn a_second_central_gets_write_not_permitted_never_an_authorization_error() {
        assert_eq!(att_error(Refusal::Busy), ReqError::NotPermitted);
        assert_eq!(att_error(Refusal::Ended), ReqError::Failed);
    }

    #[test]
    fn bluez_errors_read_as_what_the_household_can_fix() {
        let error = |kind| bluer::Error {
            kind,
            message: String::new(),
        };
        assert_eq!(bluez_reason(&error(ErrorKind::NotFound)), NO_ADAPTER);
        assert_eq!(
            bluez_reason(&error(ErrorKind::Internal(InternalErrorKind::DBus(
                SERVICE_UNKNOWN.to_string()
            )))),
            BLUETOOTHD_DOWN
        );
        assert!(bluez_reason(&error(ErrorKind::NotReady)).starts_with("BlueZ failed"));
    }

    /// Skipped where bluetoothd runs, so a unit test never registers on a real adapter; the box
    /// (no system bus) and CI (no bluetoothd) both run it.
    #[tokio::test]
    async fn start_succeeds_without_a_radio_and_the_status_says_why() {
        if Path::new("/run/dbus/system_bus_socket").exists()
            && std::process::Command::new("busctl")
                .args(["--system", "status", BLUEZ_BUS_NAME])
                .output()
                .is_ok_and(|out| out.status.success())
        {
            return;
        }
        let peripheral = BluezPeripheral::new();
        let pond = crate::test_support::test_pond();
        assert!(peripheral.start(Arc::new(pond.channel)).await.is_ok());
        let status = peripheral.status();
        assert!(
            matches!(&status, PeripheralStatus::Unavailable { reason } if !reason.is_empty()),
            "{status:?}"
        );
        let advertisement = Advertisement {
            local_name: "Pond 7f3a".into(),
        };
        assert!(matches!(
            peripheral.advertise(Some(advertisement)).await,
            Err(BleError::Unavailable(_))
        ));
        peripheral.shutdown().await;
        assert_eq!(peripheral.status(), PeripheralStatus::Off);
    }

    #[tokio::test]
    async fn an_unstarted_peripheral_is_off_and_refuses_to_advertise() {
        let peripheral = BluezPeripheral::new();
        assert_eq!(peripheral.status(), PeripheralStatus::Off);
        let advertisement = Advertisement {
            local_name: "Pond c1cd".into(),
        };
        assert!(matches!(
            peripheral.advertise(Some(advertisement)).await,
            Err(BleError::Unavailable(_))
        ));
        assert!(peripheral.advertise(None).await.is_ok());
        peripheral.shutdown().await;
        assert_eq!(peripheral.status(), PeripheralStatus::Off);
    }
}
