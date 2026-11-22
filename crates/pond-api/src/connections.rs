//! The household's connections and pairing settings (`docs/architecture/pond-connections.md`).
//! Reads take any paired token; the writes can take the pond off the network, so each asks first.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use chrono::SecondsFormat;
use pond_core::network::domain::setup_record::ClaimState;
use pond_core::network::domain::wifi::{
    is_valid_ssid, WifiForgetError, WifiJoinError, WifiPassphrase, WifiScanError,
};
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::network::ports::wired_interfaces::WiredInterfaces;
use pond_core::network::services::connection_changes::{
    may_change_connections, ChangeRefusal, NOT_ALLOWED,
};
use pond_core::network::services::wifi_connections::WifiConnections;
use pond_core::security::domain::proven_device::{DeviceRung, ProvenDevice};
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::host_guard::HostCredential;
use crate::network::{availability, NO_SETUP_CHANNEL};
use crate::AppState;

/// A join body is an SSID, a passphrase and a flag; nothing larger is a join.
pub const JOIN_BODY_LIMIT: usize = 4096;

const UNSUPPORTED: &str = "unsupported";
const INVALID_REQUEST: &str = "invalid_request";

/// What the connection routes drive, wired by the process that owns the station. Its station must
/// be the one the setup channel holds, so a join over Bluetooth and one over HTTPS never overlap.
pub struct Connections {
    wifi: WifiConnections,
    wired: Arc<dyn WiredInterfaces>,
    hostname: String,
    https_port: u16,
}

impl Connections {
    pub fn new(
        wifi: WifiConnections,
        wired: Arc<dyn WiredInterfaces>,
        hostname: impl Into<String>,
        https_port: u16,
    ) -> Self {
        Self {
            wifi,
            wired,
            hostname: hostname.into(),
            https_port,
        }
    }
}

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, code: &str) -> Refusal {
    (status, Json(json!({ "error": code })))
}

/// A server wired without a station has nothing to show or change.
fn wired(connections: Option<Extension<Arc<Connections>>>) -> Result<Arc<Connections>, Refusal> {
    connections
        .map(|Extension(connections)| connections)
        .ok_or_else(|| refuse(StatusCode::SERVICE_UNAVAILABLE, UNSUPPORTED))
}

/// Whether this caller may use the write routes. The attribution store is read only when neither
/// the host credential nor the claim already answers.
async fn change_access(
    state: &AppState,
    principal: Option<&Principal>,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
) -> Result<(), ChangeRefusal> {
    if crate::host_guard::presents_credential(credential, headers) {
        return may_change_connections(true, None, None, &DeviceRung::NoDevice);
    }
    let device = principal.map_or_else(ProvenDevice::none, ProvenDevice::from_principal);
    let claimant = match (&state.setup_claims, device.id()) {
        (Some(claims), Some(_)) => claims.claimed_by().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "connections: the claim is unreadable; nobody counts as its claimant");
            None
        }),
        _ => None,
    };
    let attribution = match device.id() {
        Some(id) if claimant.as_deref() != Some(id) => {
            pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
                state.db.system.clone(),
            )
            .device_profile(id)
            .await
        }
        _ => Ok(None),
    };
    may_change_connections(
        false,
        device.id(),
        claimant.as_deref(),
        &device.rung(attribution),
    )
}

/// The write routes' gate: `403 not_allowed`, logged with who asked and why not.
async fn require_changer(
    state: &AppState,
    principal: Option<&Principal>,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
    operation: &'static str,
) -> Result<(), Refusal> {
    change_access(state, principal, credential, headers)
        .await
        .map_err(|refusal| {
            tracing::warn!(
                kind = "connections_change_denied",
                operation,
                refusal = refusal.as_str(),
                caller = principal
                    .and_then(|p| p.device_id.as_deref())
                    .unwrap_or("none"),
                "refused to change the pond's network for this caller"
            );
            refuse(StatusCode::FORBIDDEN, NOT_ALLOWED)
        })
}

async fn ble_enabled(state: &AppState) -> Option<bool> {
    match state.settings_repo.get().await {
        Ok(settings) => Some(settings.ble_setup_enabled),
        Err(error) => {
            tracing::warn!(%error, "connections: settings unreadable");
            None
        }
    }
}

fn bluetooth(enabled: Option<bool>, radios: Option<&Arc<dyn SetupRadios>>) -> Value {
    let Some(radios) = radios else {
        return json!({"enabled": enabled, "state": "unavailable", "reason": NO_SETUP_CHANNEL,
                      "advertising_as": null, "session_open": false});
    };
    let ble = radios.ble();
    json!({
        "enabled": enabled,
        "state": ble.as_str(),
        "reason": ble.reason(),
        "advertising_as": radios.advertising_as(),
        "session_open": radios.session_open(),
    })
}

/// `GET /api/v1/connections`: Wi-Fi, Ethernet and Bluetooth at once, and whether this caller may
/// change them.
pub(crate) async fn connections(
    State(state): State<Arc<AppState>>,
    connections: Option<Extension<Arc<Connections>>>,
    radios: Option<Extension<Arc<dyn SetupRadios>>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let connections = wired(connections)?;
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    let ((link, capabilities), ethernet, access, enabled) = tokio::join!(
        connections.wifi.link(),
        connections.wired.wired_interfaces(),
        change_access(&state, principal, credential, &headers),
        ble_enabled(&state),
    );
    let radios = radios.as_ref().map(|Extension(radios)| radios);
    Ok(Json(json!({
        "hostname": connections.hostname,
        "https_port": connections.https_port,
        "wifi": {
            "state": link.state,
            "reason": link.reason,
            "ssid": link.ssid,
            "address": link.address,
            "signal": link.signal,
            "capabilities": {
                "scan": availability(&capabilities.scan),
                "join": availability(&capabilities.join),
                "forget": availability(&capabilities.forget),
            },
        },
        "ethernet": ethernet,
        "bluetooth": bluetooth(enabled, radios),
        "can_change": {
            "allowed": access.is_ok(),
            "reason": access.err().map(|refusal| refusal.to_string()),
        },
    })))
}

/// `GET /api/v1/connections/wifi/networks`: a fresh scan, ranked.
pub(crate) async fn wifi_networks(
    connections: Option<Extension<Arc<Connections>>>,
) -> Result<Json<Value>, Refusal> {
    let connections = wired(connections)?;
    match connections.wifi.networks().await {
        Ok(networks) => Ok(Json(json!({ "networks": networks }))),
        Err(error) => Err(refuse(scan_status(error), error.code())),
    }
}

/// The passphrase never reaches `Debug`, `Serialize` or a log (`WifiPassphrase`).
#[derive(Deserialize)]
struct JoinBody {
    ssid: String,
    #[serde(default)]
    passphrase: Option<WifiPassphrase>,
    #[serde(default)]
    hidden: bool,
}

/// `POST /api/v1/connections/wifi/join`: waits for the join, up to the contract's 45 s.
pub(crate) async fn wifi_join(
    State(state): State<Arc<AppState>>,
    connections: Option<Extension<Arc<Connections>>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "wifi_join").await?;
    let connections = wired(connections)?;
    // The parse error is dropped unread: for a mistyped passphrase it would quote the value.
    let request = match serde_json::from_slice::<JoinBody>(&body) {
        Ok(request) if is_valid_ssid(&request.ssid) => request,
        _ => return Err(refuse(StatusCode::BAD_REQUEST, INVALID_REQUEST)),
    };
    let JoinBody {
        ssid,
        passphrase,
        hidden,
    } = request;
    tracing::info!(
        %ssid,
        hidden,
        with_passphrase = passphrase.is_some(),
        caller = principal.and_then(|p| p.device_id.as_deref()).unwrap_or("host"),
        "joining Wi-Fi for the household's connections settings"
    );
    match connections.wifi.join(ssid, passphrase, hidden).await {
        Ok(joined) => Ok(Json(json!({
            "ssid": joined.ssid,
            "address": joined.ipv4.to_string(),
            "https_port": connections.https_port,
            "hostname": connections.hostname,
        }))),
        Err(error) => Err(refuse(join_status(error), error.code())),
    }
}

#[derive(Deserialize)]
pub(crate) struct ForgetQuery {
    #[serde(default)]
    confirm: bool,
}

/// `DELETE /api/v1/connections/wifi/networks/{ssid}[?confirm=true]`.
pub(crate) async fn wifi_forget(
    State(state): State<Arc<AppState>>,
    connections: Option<Extension<Arc<Connections>>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    ssid: Result<Path<String>, PathRejection>,
    query: Result<Query<ForgetQuery>, QueryRejection>,
) -> Result<StatusCode, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "wifi_forget").await?;
    let connections = wired(connections)?;
    let (Ok(Path(ssid)), Ok(Query(query))) = (ssid, query) else {
        return Err(refuse(StatusCode::BAD_REQUEST, INVALID_REQUEST));
    };
    if !is_valid_ssid(&ssid) {
        return Err(refuse(StatusCode::BAD_REQUEST, INVALID_REQUEST));
    }
    match connections.wifi.forget(&ssid, query.confirm).await {
        Ok(()) => {
            tracing::info!(
                %ssid,
                confirmed = query.confirm,
                caller = principal.and_then(|p| p.device_id.as_deref()).unwrap_or("host"),
                "forgot a saved Wi-Fi network for the household's connections settings"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(error) => Err(refuse(forget_status(error), error.code())),
    }
}

/// `GET /api/v1/setup/status`: the setup card's state for this caller, never its code.
pub(crate) async fn setup_status(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Refusal> {
    let device = principal
        .as_ref()
        .map_or_else(ProvenDevice::none, |Extension(p)| {
            ProvenDevice::from_principal(p)
        });
    let status = match &state.setup_claims {
        Some(claims) => claims.setup_status().await.map_err(|error| {
            tracing::warn!(%error, "setup status: the setup record is unreadable");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        })?,
        None => None,
    };
    let Some(status) = status else {
        return Ok(Json(
            json!({"id": null, "claimed": false, "claimed_at": null,
                              "claimed_by_this_device": false}),
        ));
    };
    let claimed_at = match status.claim {
        ClaimState::Claimed { at } => Some(at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        ClaimState::Unclaimed => None,
    };
    Ok(Json(json!({
        "id": status.discriminator.to_string(),
        "claimed": status.claim.is_claimed(),
        "claimed_at": claimed_at,
        "claimed_by_this_device": status.claimed_by(device.id()),
    })))
}

fn scan_status(error: WifiScanError) -> StatusCode {
    match error {
        WifiScanError::NotPermitted => StatusCode::FORBIDDEN,
        WifiScanError::Unsupported => StatusCode::SERVICE_UNAVAILABLE,
        WifiScanError::Busy => StatusCode::CONFLICT,
        WifiScanError::Timeout => StatusCode::GATEWAY_TIMEOUT,
    }
}

fn join_status(error: WifiJoinError) -> StatusCode {
    match error {
        // Not 401: every client reads 401 as an expired token and would re-pair, then resend.
        WifiJoinError::WrongPassword => StatusCode::UNPROCESSABLE_ENTITY,
        WifiJoinError::NotFound => StatusCode::NOT_FOUND,
        WifiJoinError::NoAddress => StatusCode::BAD_GATEWAY,
        WifiJoinError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        WifiJoinError::NotPermitted => StatusCode::FORBIDDEN,
        WifiJoinError::Unsupported => StatusCode::SERVICE_UNAVAILABLE,
        WifiJoinError::Busy => StatusCode::CONFLICT,
        WifiJoinError::UnsupportedSecurity => StatusCode::BAD_REQUEST,
    }
}

fn forget_status(error: WifiForgetError) -> StatusCode {
    match error {
        WifiForgetError::NotFound => StatusCode::NOT_FOUND,
        WifiForgetError::InUse => StatusCode::CONFLICT,
        WifiForgetError::NotPermitted => StatusCode::FORBIDDEN,
        WifiForgetError::Unsupported => StatusCode::SERVICE_UNAVAILABLE,
        WifiForgetError::Busy => StatusCode::CONFLICT,
    }
}

fn valid_mac(raw: &str) -> Option<String> {
    let mac = raw.trim().to_ascii_uppercase();
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit())) {
        Some(mac)
    } else {
        None
    }
}

async fn run_bluetoothctl(args: &[&str]) -> Result<String, Refusal> {
    let out = tokio::process::Command::new("bluetoothctl")
        .args(args)
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "bluetoothctl unavailable");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "bluetooth_unavailable")
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        tracing::warn!(?args, %stdout, %stderr, "bluetoothctl command failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn bt_adapter_status() -> (bool, bool, bool) {
    let info = run_bluetoothctl(&["show"]).await.unwrap_or_default();
    let powered = info.contains("Powered: yes");
    let discoverable = info.contains("Discoverable: yes");
    let pairable = info.contains("Pairable: yes");
    (powered, discoverable, pairable)
}

fn is_mac_like(name: &str, addr: &str) -> bool {
    let norm_name: String = name
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let norm_addr: String = addr
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    norm_name.is_empty() || norm_name == norm_addr
}

fn infer_bt_type(info: &str, name: &str) -> &'static str {
    let icon = info
        .lines()
        .find_map(|l| l.trim().strip_prefix("Icon: ").map(str::trim))
        .unwrap_or("");
    match icon {
        "audio-headset" => return "Headset",
        "audio-headphones" => return "Headphones",
        "audio-card" | "audio-speakers" => return "Speaker",
        "input-keyboard" => return "Keyboard",
        "input-mouse" => return "Mouse",
        "input-gaming" => return "Controller",
        "phone" => return "Phone",
        "computer" => return "Computer",
        "camera-photo" | "camera-video" => return "Camera",
        "printer" => return "Printer",
        _ => {}
    }

    let lower_info = info.to_ascii_lowercase();
    let lower_name = name.to_ascii_lowercase();

    if lower_info.contains("0000110b") || lower_info.contains("0000110a") || lower_name.contains("speaker") || lower_name.contains("soundbar") {
        return "Speaker";
    }
    if lower_info.contains("0000111e") || lower_info.contains("00001108") || lower_name.contains("buds") || lower_name.contains("airpods") || lower_name.contains("headphone") || lower_name.contains("headset") {
        return "Headphones";
    }
    if lower_info.contains("00001124") || lower_info.contains("00001812") {
        if lower_name.contains("mouse") {
            return "Mouse";
        }
        if lower_name.contains("controller") || lower_name.contains("gamepad") {
            return "Controller";
        }
        return "Keyboard / HID";
    }
    if lower_info.contains("185b51bd") || lower_name.starts_with("pond ") {
        return "Pond";
    }
    if lower_info.contains("0000180d") || lower_info.contains("0000181a") || lower_name.contains("sensor") || lower_name.contains("watch") || lower_name.contains("band") {
        return "Sensor / Wearable";
    }
    if let Some(class_hex) = info
        .lines()
        .find_map(|l| l.trim().strip_prefix("Class: 0x"))
        .and_then(|h| u32::from_str_radix(h.trim(), 16).ok())
    {
        match (class_hex >> 8) & 0x1f {
            0x01 => return "Computer",
            0x02 => return "Phone",
            0x04 => return "Audio / Video",
            0x05 => return "Accessory (HID)",
            0x06 => return "Imaging / Camera",
            0x07 => return "Wearable",
            0x09 => return "Health Sensor",
            _ => {}
        }
    }
    if lower_name.contains("iphone") || lower_name.contains("galaxy") || lower_name.contains("pixel") || lower_name.contains("phone") {
        return "Phone";
    }
    if lower_name.contains("macbook") || lower_name.contains("laptop") || lower_name.contains("pc") {
        return "Computer";
    }
    if lower_name.contains("tv") || lower_name.contains("display") {
        return "TV / Display";
    }
    "Accessory"
}

fn bt_display_name(addr: &str, list_name: &str, info: &str, device_type: &str) -> (String, bool) {
    let info_name = info
        .lines()
        .find_map(|l| l.trim().strip_prefix("Name: ").map(str::trim));
    let info_alias = info
        .lines()
        .find_map(|l| l.trim().strip_prefix("Alias: ").map(str::trim));

    for candidate in [info_name, Some(list_name), info_alias].into_iter().flatten() {
        let trimmed = candidate.trim();
        if !is_mac_like(trimmed, addr) {
            return (trimmed.to_string(), true);
        }
    }
    (format!("Unnamed {device_type}"), false)
}

async fn list_bt_devices() -> Result<Vec<Value>, Refusal> {
    let raw = run_bluetoothctl(&["devices"]).await?;
    let mut parsed = Vec::new();
    for line in raw.lines() {
        let Some(rest) = line.strip_prefix("Device ") else {
            continue;
        };
        let mut parts = rest.splitn(2, ' ');
        let Some(addr) = parts.next().and_then(valid_mac) else {
            continue;
        };
        let name = parts.next().unwrap_or(&addr).trim().to_string();
        parsed.push((addr, name));
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, (addr, list_name)) in parsed.into_iter().enumerate() {
        set.spawn(async move {
            let info = run_bluetoothctl(&["info", &addr]).await.unwrap_or_default();
            let paired = info.contains("Paired: yes");
            let connected = info.contains("Connected: yes");
            let icon = info
                .lines()
                .find_map(|l| l.trim().strip_prefix("Icon: ").map(str::trim))
                .map(str::to_string);
            let provisional_type = infer_bt_type(&info, &list_name);
            let (name, has_name) = bt_display_name(&addr, &list_name, &info, provisional_type);
            let device_type = infer_bt_type(&info, &name);
            (
                idx,
                has_name,
                json!({
                    "address": addr,
                    "name": name,
                    "device_type": device_type,
                    "paired": paired,
                    "connected": connected,
                    "icon": icon,
                }),
            )
        });
    }

    let mut indexed = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(item) = res {
            indexed.push(item);
        }
    }
    indexed.sort_by_key(|(idx, has_name, val)| {
        let connected = val["connected"].as_bool().unwrap_or(false);
        let paired = val["paired"].as_bool().unwrap_or(false);
        let specific_type = val["device_type"].as_str() != Some("Accessory");
        (!connected, !paired, !has_name, !specific_type, *idx)
    });
    Ok(indexed.into_iter().map(|(_, _, v)| v).collect())
}

/// `GET /api/v1/connections/bluetooth/devices`: adapter state + known & discovered Bluetooth devices.
pub(crate) async fn bluetooth_devices() -> Result<Json<Value>, Refusal> {
    let ((powered, discoverable, pairable), devices) =
        tokio::join!(bt_adapter_status(), list_bt_devices());
    Ok(Json(json!({
        "powered": powered,
        "discoverable": discoverable,
        "pairable": pairable,
        "devices": devices?,
    })))
}

/// `POST /api/v1/connections/bluetooth/scan`: scan for nearby Bluetooth devices & accessories.
pub(crate) async fn bluetooth_scan(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "bluetooth_scan").await?;
    let _ = run_bluetoothctl(&["power", "on"]).await;
    let _ = run_bluetoothctl(&["--timeout", "4", "scan", "on"]).await;
    let ((powered, discoverable, pairable), devices) =
        tokio::join!(bt_adapter_status(), list_bt_devices());
    Ok(Json(json!({
        "powered": powered,
        "discoverable": discoverable,
        "pairable": pairable,
        "devices": devices?,
    })))
}

#[derive(Deserialize)]
pub(crate) struct BluetoothPowerBody {
    powered: Option<bool>,
    discoverable: Option<bool>,
    pairable: Option<bool>,
}

/// `POST /api/v1/connections/bluetooth/power`: toggle Bluetooth adapter power, discoverability, or pairing mode.
pub(crate) async fn bluetooth_power(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Json(body): Json<BluetoothPowerBody>,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "bluetooth_power").await?;
    if let Some(on) = body.powered {
        let _ = run_bluetoothctl(&["power", if on { "on" } else { "off" }]).await?;
    }
    if let Some(on) = body.discoverable {
        if on {
            let _ = run_bluetoothctl(&["discoverable-timeout", "0"]).await;
        }
        let _ = run_bluetoothctl(&["discoverable", if on { "on" } else { "off" }]).await?;
    }
    if let Some(on) = body.pairable {
        if let Err(err) = run_bluetoothctl(&["pairable", if on { "on" } else { "off" }]).await {
            tracing::warn!(?err, on, "bluetoothctl pairable failed; continuing");
        }
    }
    let (powered, discoverable, pairable) = bt_adapter_status().await;
    Ok(Json(json!({
        "powered": powered,
        "discoverable": discoverable,
        "pairable": pairable,
    })))
}

#[derive(Deserialize)]
pub(crate) struct BluetoothAddressBody {
    address: String,
}

/// `POST /api/v1/connections/bluetooth/connect`: pair, trust, and connect a Bluetooth device.
pub(crate) async fn bluetooth_connect(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Json(body): Json<BluetoothAddressBody>,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "bluetooth_connect").await?;
    let addr = valid_mac(&body.address)
        .ok_or_else(|| refuse(StatusCode::UNPROCESSABLE_ENTITY, "invalid_address"))?;
    let _ = run_bluetoothctl(&["power", "on"]).await;
    let _ = run_bluetoothctl(&["agent", "NoInputNoOutput"]).await;
    let _ = run_bluetoothctl(&["default-agent"]).await;
    let _ = run_bluetoothctl(&["pairable", "on"]).await;
    let _ = run_bluetoothctl(&["pair", &addr]).await;
    let _ = run_bluetoothctl(&["trust", &addr]).await;
    let out = run_bluetoothctl(&["connect", &addr]).await.unwrap_or_default();
    let connected = out.contains("Connection successful")
        || run_bluetoothctl(&["info", &addr])
            .await
            .is_ok_and(|info| info.contains("Connected: yes"));
    if !connected {
        return Err(refuse(StatusCode::BAD_GATEWAY, "bluetooth_connect_failed"));
    }
    Ok(Json(json!({ "connected": true, "address": addr })))
}

/// `POST /api/v1/connections/bluetooth/disconnect`: disconnect a Bluetooth device.
pub(crate) async fn bluetooth_disconnect(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Json(body): Json<BluetoothAddressBody>,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "bluetooth_disconnect").await?;
    let addr = valid_mac(&body.address)
        .ok_or_else(|| refuse(StatusCode::UNPROCESSABLE_ENTITY, "invalid_address"))?;
    let _ = run_bluetoothctl(&["disconnect", &addr]).await?;
    Ok(Json(json!({ "connected": false, "address": addr })))
}

/// `DELETE /api/v1/connections/bluetooth/devices/{address}`: unpair/remove a Bluetooth device.
pub(crate) async fn bluetooth_forget(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Path(address): Path<String>,
) -> Result<Json<Value>, Refusal> {
    let principal = principal.as_ref().map(|Extension(p)| p);
    let credential = credential.as_ref().map(|Extension(c)| c);
    require_changer(&state, principal, credential, &headers, "bluetooth_forget").await?;
    let addr = valid_mac(&address)
        .ok_or_else(|| refuse(StatusCode::UNPROCESSABLE_ENTITY, "invalid_address"))?;
    let _ = run_bluetoothctl(&["remove", &addr]).await?;
    Ok(Json(json!({ "removed": true, "address": addr })))
}
