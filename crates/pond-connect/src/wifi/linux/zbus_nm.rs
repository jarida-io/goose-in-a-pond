//! [`NetworkManager`] over the system bus with zbus. Proxies are built per call and uncached: the
//! station runs a few calls a minute at most, so a property cache would only add match rules.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::Mutex;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

use super::nm::{
    AccessPoint, DeviceLookup, DeviceState, JoinRequest, NetworkManager, NmError, SavedConnection,
    StateChange, WifiDevice, DEVICE_TYPE_WIFI,
};

const SERVICE: &str = "org.freedesktop.NetworkManager";
const ACCESS_POINT_INTERFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";
const NAME_HAS_NO_OWNER: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";
const NM_DOWN: &str = "NetworkManager is not running, so this pond cannot manage its Wi-Fi";
const NO_PATH: &str = "/";

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager",
    gen_blocking = false
)]
trait Manager {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    fn get_permissions(&self) -> zbus::Result<HashMap<String, String>>;

    fn activate_connection(
        &self,
        connection: &ObjectPath<'_>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;

    #[zbus(name = "AddAndActivateConnection2")]
    fn add_and_activate_connection2(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<(
        OwnedObjectPath,
        OwnedObjectPath,
        HashMap<String, OwnedValue>,
    )>;

    fn deactivate_connection(&self, active_connection: &ObjectPath<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_wireless_enabled(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager",
    gen_blocking = false
)]
trait Device {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn managed(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn state_reason(&self) -> zbus::Result<(u32, u32)>;

    #[zbus(property, name = "Ip4Config")]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(property)]
    fn active_connection(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(signal)]
    fn state_changed(&self, new_state: u32, old_state: u32, reason: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager",
    gen_blocking = false
)]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    fn get_all_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property)]
    fn last_scan(&self) -> zbus::Result<i64>;

    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager",
    gen_blocking = false
)]
trait AccessPointProps {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings",
    gen_blocking = false
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager",
    gen_blocking = false
)]
trait SettingsConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;

    fn save(&self) -> zbus::Result<()>;

    fn delete(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager",
    gen_blocking = false
)]
trait Ip4Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

/// The system bus, dialled on first use and again after it drops.
#[derive(Default)]
pub(super) struct ZbusNetworkManager {
    bus: Mutex<Option<Connection>>,
}

impl ZbusNetworkManager {
    pub(super) fn new() -> Self {
        Self::default()
    }

    async fn bus(&self) -> Result<Connection, NmError> {
        let mut bus = self.bus.lock().await;
        if let Some(connection) = bus.as_ref() {
            return Ok(connection.clone());
        }
        let connection = Connection::system().await.map_err(|error| {
            NmError::Unreachable(format!("cannot reach the D-Bus system bus: {error}"))
        })?;
        *bus = Some(connection.clone());
        Ok(connection)
    }

    /// Forgets a connection that failed at the transport, so the next call dials again.
    async fn checked<T>(&self, result: zbus::Result<T>) -> Result<T, NmError> {
        result.map_err(|error| {
            let mapped = nm_error(&error);
            if matches!(error, zbus::Error::InputOutput(_)) {
                if let Ok(mut bus) = self.bus.try_lock() {
                    *bus = None;
                }
            }
            mapped
        })
    }

    async fn manager(&self) -> Result<ManagerProxy<'static>, NmError> {
        let bus = self.bus().await?;
        let proxy = ManagerProxy::builder(&bus)
            .cache_properties(CacheProperties::No)
            .build()
            .await;
        self.checked(proxy).await
    }

    async fn device(&self, path: &str) -> Result<DeviceProxy<'static>, NmError> {
        let bus = self.bus().await?;
        let proxy = async {
            DeviceProxy::builder(&bus)
                .path(path.to_string())?
                .cache_properties(CacheProperties::No)
                .build()
                .await
        }
        .await;
        self.checked(proxy).await
    }

    async fn wireless(&self, path: &str) -> Result<WirelessProxy<'static>, NmError> {
        let bus = self.bus().await?;
        let proxy = async {
            WirelessProxy::builder(&bus)
                .path(path.to_string())?
                .cache_properties(CacheProperties::No)
                .build()
                .await
        }
        .await;
        self.checked(proxy).await
    }

    async fn connection(&self, path: &str) -> Result<SettingsConnectionProxy<'static>, NmError> {
        let bus = self.bus().await?;
        let proxy = async {
            SettingsConnectionProxy::builder(&bus)
                .path(path.to_string())?
                .cache_properties(CacheProperties::No)
                .build()
                .await
        }
        .await;
        self.checked(proxy).await
    }

    async fn access_point(&self, bus: &Connection, path: &str) -> Option<AccessPoint> {
        let properties = zbus::fdo::PropertiesProxy::builder(bus)
            .destination(SERVICE)
            .ok()?
            .path(path.to_string())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .ok()?;
        let interface = zbus::names::InterfaceName::try_from(ACCESS_POINT_INTERFACE).ok()?;
        let all = properties.get_all(interface).await.ok()?;
        let number = |key: &str| all.get(key).and_then(|v| u32::try_from(v).ok());
        Some(AccessPoint {
            path: path.to_string(),
            ssid: all.get("Ssid").and_then(bytes)?,
            flags: number("Flags")?,
            wpa_flags: number("WpaFlags")?,
            rsn_flags: number("RsnFlags")?,
            strength: all.get("Strength").and_then(|v| u8::try_from(v).ok())?,
        })
    }
}

#[async_trait]
impl NetworkManager for ZbusNetworkManager {
    async fn wifi_device(&self) -> Result<DeviceLookup, NmError> {
        let manager = self.manager().await?;
        let mut paths = self.checked(manager.get_devices().await).await?;
        paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut unmanaged = None;
        for path in paths {
            let device = self.device(path.as_str()).await?;
            if self.checked(device.device_type().await).await? != DEVICE_TYPE_WIFI {
                continue;
            }
            let interface = self.checked(device.interface().await).await?;
            if self.checked(device.managed().await).await? {
                return Ok(DeviceLookup::Found(WifiDevice {
                    path: path.to_string(),
                    interface,
                }));
            }
            unmanaged.get_or_insert(interface);
        }
        Ok(unmanaged.map_or(DeviceLookup::Missing, DeviceLookup::Unmanaged))
    }

    async fn permissions(&self) -> Result<HashMap<String, String>, NmError> {
        let manager = self.manager().await?;
        self.checked(manager.get_permissions().await).await
    }

    async fn wireless_enabled(&self) -> Result<(bool, bool), NmError> {
        let manager = self.manager().await?;
        let software = self.checked(manager.wireless_enabled().await).await?;
        let hardware = self
            .checked(manager.wireless_hardware_enabled().await)
            .await?;
        Ok((software, hardware))
    }

    async fn enable_wireless(&self) -> Result<(), NmError> {
        let manager = self.manager().await?;
        self.checked(manager.set_wireless_enabled(true).await).await
    }

    async fn request_scan(&self, device: &str) -> Result<(), NmError> {
        let wireless = self.wireless(device).await?;
        self.checked(wireless.request_scan(HashMap::new()).await)
            .await
    }

    async fn last_scan(&self, device: &str) -> Result<i64, NmError> {
        let wireless = self.wireless(device).await?;
        self.checked(wireless.last_scan().await).await
    }

    async fn access_points(&self, device: &str) -> Result<Vec<AccessPoint>, NmError> {
        let wireless = self.wireless(device).await?;
        let paths = self.checked(wireless.get_all_access_points().await).await?;
        let bus = self.bus().await?;
        // An access point can vanish between the listing and its read; it is left out.
        let reads = paths
            .iter()
            .map(|path| self.access_point(&bus, path.as_str()));
        Ok(futures::future::join_all(reads)
            .await
            .into_iter()
            .flatten()
            .collect())
    }

    async fn saved_connections(&self) -> Result<Vec<SavedConnection>, NmError> {
        let bus = self.bus().await?;
        let settings = self
            .checked(
                SettingsProxy::builder(&bus)
                    .cache_properties(CacheProperties::No)
                    .build()
                    .await,
            )
            .await?;
        let paths = self.checked(settings.list_connections().await).await?;
        let mut saved = Vec::new();
        for path in paths {
            let Ok(connection) = self.connection(path.as_str()).await else {
                continue;
            };
            let Ok(settings) = connection.get_settings().await else {
                continue;
            };
            if let Some(ssid) = settings
                .get("802-11-wireless")
                .and_then(|wireless| wireless.get("ssid"))
                .and_then(bytes)
            {
                saved.push(SavedConnection {
                    path: path.to_string(),
                    ssid,
                });
            }
        }
        Ok(saved)
    }

    async fn device_state(&self, device: &str) -> Result<DeviceState, NmError> {
        let proxy = self.device(device).await?;
        let (state, reason) = self.checked(proxy.state_reason().await).await?;
        let active = self.checked(proxy.active_connection().await).await?;
        Ok(DeviceState {
            state,
            reason,
            active_connection: real_path(&active),
        })
    }

    async fn active_ssid(&self, device: &str) -> Result<Option<Vec<u8>>, NmError> {
        let wireless = self.wireless(device).await?;
        let path = self.checked(wireless.active_access_point().await).await?;
        let Some(path) = real_path(&path) else {
            return Ok(None);
        };
        let bus = self.bus().await?;
        let access_point = async {
            AccessPointPropsProxy::builder(&bus)
                .path(path)?
                .cache_properties(CacheProperties::No)
                .build()
                .await
        }
        .await;
        let access_point = self.checked(access_point).await?;
        Ok(Some(self.checked(access_point.ssid().await).await?))
    }

    async fn ipv4(&self, device: &str) -> Result<Option<Ipv4Addr>, NmError> {
        let proxy = self.device(device).await?;
        let config = self.checked(proxy.ip4_config().await).await?;
        let Some(config) = real_path(&config) else {
            return Ok(None);
        };
        let bus = self.bus().await?;
        let config = async {
            Ip4ConfigProxy::builder(&bus)
                .path(config)?
                .cache_properties(CacheProperties::No)
                .build()
                .await
        }
        .await;
        let config = self.checked(config).await?;
        let addresses = self.checked(config.address_data().await).await?;
        Ok(addresses.iter().find_map(|address| {
            address
                .get("address")
                .and_then(|value| <&str>::try_from(value).ok())
                .and_then(|text| text.parse().ok())
        }))
    }

    async fn state_changes(
        &self,
        device: &str,
    ) -> Result<BoxStream<'static, StateChange>, NmError> {
        let proxy = self.device(device).await?;
        let signals = self.checked(proxy.receive_state_changed().await).await?;
        Ok(signals
            .filter_map(|signal| async move {
                let args = signal.args().ok()?;
                Some(StateChange {
                    new: args.new_state,
                    reason: args.reason,
                })
            })
            .boxed())
    }

    async fn add_and_activate(
        &self,
        request: &JoinRequest<'_>,
        device: &str,
        access_point: Option<&str>,
    ) -> Result<(String, String), NmError> {
        let manager = self.manager().await?;
        let device = object_path(device)?;
        let access_point = object_path(access_point.unwrap_or(NO_PATH))?;
        // In memory until it works: a wrong passphrase never reaches disk or autoconnects.
        let options = HashMap::from([("persist", Value::from("memory"))]);
        let (connection, active, _) = self
            .checked(
                manager
                    .add_and_activate_connection2(
                        connection_settings(request),
                        &device,
                        &access_point,
                        options,
                    )
                    .await,
            )
            .await?;
        Ok((connection.to_string(), active.to_string()))
    }

    async fn activate(
        &self,
        connection: &str,
        device: &str,
        access_point: Option<&str>,
    ) -> Result<String, NmError> {
        let manager = self.manager().await?;
        let active = self
            .checked(
                manager
                    .activate_connection(
                        &object_path(connection)?,
                        &object_path(device)?,
                        &object_path(access_point.unwrap_or(NO_PATH))?,
                    )
                    .await,
            )
            .await?;
        Ok(active.to_string())
    }

    async fn deactivate(&self, active_connection: &str) -> Result<(), NmError> {
        let manager = self.manager().await?;
        self.checked(
            manager
                .deactivate_connection(&object_path(active_connection)?)
                .await,
        )
        .await
    }

    async fn save(&self, connection: &str) -> Result<(), NmError> {
        let proxy = self.connection(connection).await?;
        self.checked(proxy.save().await).await
    }

    async fn delete(&self, connection: &str) -> Result<(), NmError> {
        let proxy = self.connection(connection).await?;
        self.checked(proxy.delete().await).await
    }
}

/// The connection NetworkManager completes and activates. The passphrase is borrowed into the
/// message, never copied into a `String` here.
fn connection_settings<'a>(
    request: &'a JoinRequest<'a>,
) -> HashMap<&'static str, HashMap<&'static str, Value<'a>>> {
    let mut wireless = HashMap::from([
        ("ssid", Value::from(request.ssid.as_bytes())),
        ("mode", Value::from("infrastructure")),
    ]);
    if request.hidden {
        wireless.insert("hidden", Value::from(true));
    }
    let mut settings = HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from(request.ssid)),
                ("type", Value::from("802-11-wireless")),
            ]),
        ),
        ("802-11-wireless", wireless),
        ("ipv4", HashMap::from([("method", Value::from("auto"))])),
        ("ipv6", HashMap::from([("method", Value::from("auto"))])),
    ]);
    if let Some(key_mgmt) = request.key_mgmt {
        let mut security = HashMap::from([("key-mgmt", Value::from(key_mgmt.as_str()))]);
        if let Some(passphrase) = request.passphrase.filter(|_| key_mgmt.needs_passphrase()) {
            security.insert("psk", Value::from(passphrase));
        }
        settings.insert("802-11-wireless-security", security);
    }
    settings
}

fn object_path(path: &str) -> Result<ObjectPath<'_>, NmError> {
    ObjectPath::try_from(path)
        .map_err(|_| NmError::Refused("org.freedesktop.DBus.Error.InvalidArgs".to_string()))
}

/// D-Bus spells "none" as the root path.
fn real_path(path: &OwnedObjectPath) -> Option<String> {
    (path.as_str() != NO_PATH).then(|| path.to_string())
}

fn bytes(value: &OwnedValue) -> Option<Vec<u8>> {
    Vec::<u8>::try_from(value.try_clone().ok()?).ok()
}

fn nm_error(error: &zbus::Error) -> NmError {
    let name = match error {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        zbus::Error::FDO(error) => zbus::DBusError::name(error.as_ref()).to_string(),
        other => {
            return NmError::Unreachable(format!("NetworkManager could not be asked: {other}"))
        }
    };
    if name == SERVICE_UNKNOWN || name == NAME_HAS_NO_OWNER {
        NmError::Unreachable(NM_DOWN.to_string())
    } else {
        NmError::Refused(name)
    }
}

#[cfg(test)]
mod tests {
    use super::super::nm::KeyMgmt;
    use super::*;

    fn text<'a>(value: Option<&'a Value<'_>>) -> Option<&'a str> {
        match value? {
            Value::Str(text) => Some(text.as_str()),
            _ => None,
        }
    }

    #[test]
    fn a_wpa_join_carries_the_passphrase_and_an_open_one_has_no_security_section() {
        let request = JoinRequest {
            ssid: "Home",
            key_mgmt: Some(KeyMgmt::WpaPsk),
            passphrase: Some("otter-lantern-quietly-91"),
            hidden: false,
        };
        let settings = connection_settings(&request);
        let security = &settings["802-11-wireless-security"];
        assert_eq!(text(security.get("key-mgmt")), Some("wpa-psk"));
        assert_eq!(text(security.get("psk")), Some("otter-lantern-quietly-91"));
        assert_eq!(
            text(settings["connection"].get("type")),
            Some("802-11-wireless")
        );
        assert_eq!(text(settings["ipv4"].get("method")), Some("auto"));
        assert!(!settings["802-11-wireless"].contains_key("hidden"));
        assert_eq!(
            settings["802-11-wireless"]["ssid"],
            Value::from(b"Home".as_slice()),
            "an SSID travels as bytes"
        );

        let open = JoinRequest {
            ssid: "Cafe",
            key_mgmt: None,
            passphrase: None,
            hidden: true,
        };
        let settings = connection_settings(&open);
        assert!(!settings.contains_key("802-11-wireless-security"));
        assert_eq!(settings["802-11-wireless"]["hidden"], Value::from(true));
    }

    #[test]
    fn enhanced_open_sends_no_passphrase_even_if_the_phone_gave_one() {
        let request = JoinRequest {
            ssid: "Library",
            key_mgmt: Some(KeyMgmt::Owe),
            passphrase: Some("unused-but-given"),
            hidden: false,
        };
        let settings = connection_settings(&request);
        assert_eq!(
            text(settings["802-11-wireless-security"].get("key-mgmt")),
            Some("owe")
        );
        assert!(!settings["802-11-wireless-security"].contains_key("psk"));
    }

    #[test]
    fn root_is_no_object() {
        let root = OwnedObjectPath::try_from("/").unwrap();
        let active =
            OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/ActiveConnection/3")
                .unwrap();
        assert_eq!(real_path(&root), None);
        assert_eq!(
            real_path(&active).as_deref(),
            Some("/org/freedesktop/NetworkManager/ActiveConnection/3")
        );
    }
}
