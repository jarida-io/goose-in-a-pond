//! Adapter tests against an in-process mock `giap-matter` WebSocket controller. Verb-to-cluster
//! mapping is the controller's half, tested in `matter-server/test/control.test.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use pond_core::shared::ports::event_bus::BusStream;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
use pond_core::user_data::ports::device_commissioning::{DeviceCommissioningPort, SetupCode};
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::matter_runtime::{
    MatterConfig, MatterRuntimePort, MatterState, MatterStatus,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

use crate::bridge::{
    run_matter_bridge, run_matter_bridge_with_cache, run_matter_supervisor, ReadingCache,
    SupervisorConfig,
};
use crate::client::MatterClient;
use crate::commissioning::MatterCommissioner;
use crate::control::{MatterDeviceControl, SharedMatterClient};
use crate::notify::MatterNotifier;
use crate::runtime::MatterRuntime;

// ── Mock controller ──────────────────────────────────────────────────────────

/// Every request the mock received, for assertions.
type Received = Arc<Mutex<Vec<Value>>>;

fn greeting() -> Message {
    Message::Text(
        json!({
            "protocol": "giap-matter",
            "version": 1,
            "fabric_id": 1,
            "matter_js": "test",
        })
        .to_string()
        .into(),
    )
}

/// How the mock should answer one op.
type Answer = Arc<dyn Fn(&Value) -> Value + Send + Sync>;

/// `apply` config with BLE off; no test here is about the transport.
fn ip_only(url: impl Into<String>) -> MatterConfig {
    MatterConfig {
        url: url.into(),
        ble: false,
    }
}

/// Mock controller: greets, answers `subscribe` with `snapshot`, pushes `events`, else `answer`.
/// Accepts in a loop, tolerating failed handshakes: `is_running` probes with a bare TCP connect.
async fn mock_controller(
    snapshot: Value,
    events: Vec<Value>,
    answer: Option<Answer>,
) -> (String, Received) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/giap", listener.local_addr().unwrap());
    let received: Received = Arc::new(Mutex::new(Vec::new()));
    let received_srv = received.clone();

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let snapshot = snapshot.clone();
            let events = events.clone();
            let answer = answer.clone();
            let received_conn = received_srv.clone();

            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return; // a liveness probe, not a client
                };
                if ws.send(greeting()).await.is_err() {
                    return;
                }

                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    let id = frame["id"].as_str().unwrap().to_string();
                    received_conn.lock().unwrap().push(frame.clone());

                    if frame["op"] == "subscribe" {
                        ws.send(Message::Text(
                            json!({"id": id, "ok": true, "result": snapshot})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                        for event in &events {
                            ws.send(Message::Text(event.to_string().into()))
                                .await
                                .unwrap();
                        }
                        continue;
                    }

                    let mut reply = match &answer {
                        Some(f) => f(&frame),
                        None => json!({"id": id, "ok": true, "result": {}}),
                    };
                    reply["id"] = json!(id);
                    ws.send(Message::Text(reply.to_string().into()))
                        .await
                        .unwrap();
                }
            });
        }
    });

    (url, received)
}

/// Drops the first connection after its `subscribe`; returns the url and a `subscribe` count.
async fn mock_reconnecting_controller(snapshot: Value) -> (String, Arc<Mutex<u32>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/giap", listener.local_addr().unwrap());
    let subscribes = Arc::new(Mutex::new(0u32));
    let counter = subscribes.clone();

    tokio::spawn(async move {
        let mut conn = 0u32;
        while let Ok((stream, _)) = listener.accept().await {
            conn += 1;
            let drop_after_subscribe = conn == 1;
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(greeting()).await.unwrap();

            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let frame: Value = serde_json::from_str(&text).unwrap();
                let id = frame["id"].as_str().unwrap().to_string();
                if frame["op"] == "subscribe" {
                    *counter.lock().unwrap() += 1;
                    ws.send(Message::Text(
                        json!({"id": id, "ok": true, "result": snapshot})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                    if drop_after_subscribe {
                        let _ = ws.close(None).await;
                        break;
                    }
                } else {
                    ws.send(Message::Text(
                        json!({"id": id, "ok": true, "result": {}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            }
        }
    });

    (url, subscribes)
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn light() -> Value {
    json!({
        "id": "matter-2",
        "name": "Kitchen Light",
        "device_type": "light",
        "capabilities": ["power", "brightness"],
        "online": true,
    })
}

fn sensor() -> Value {
    json!({
        "id": "matter-4",
        "name": "Hall Sensor",
        "device_type": "sensor",
        "capabilities": [],
        "online": true,
    })
}

fn snapshot(devices: Vec<Value>, readings: Vec<Value>) -> Value {
    json!({ "devices": devices, "readings": readings })
}

fn reading(device: &str, sensor_type: &str, value: f64) -> Value {
    json!({
        "device_id": device,
        "sensor_type": sensor_type,
        "value": value,
        "unit": "bool",
    })
}

// ── Test registry ────────────────────────────────────────────────────────────

#[derive(Default)]
struct MockRegistry {
    devices: RwLock<HashMap<String, Device>>,
    heartbeats: Mutex<Vec<String>>,
    retypes: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl DeviceRegistry for MockRegistry {
    async fn register(&self, request: RegisterDeviceRequest) -> Result<Device> {
        let id = request
            .id
            .clone()
            .unwrap_or_else(|| "generated".to_string());
        let device = Device {
            id: id.clone(),
            name: request.name,
            device_type: request.device_type,
            hostname: request.hostname,
            ip_address: None,
            capabilities: request.capabilities,
            registered_at: chrono::Utc::now().to_rfc3339(),
            last_seen: None,
            is_online: true,
            room: request.room,
        };
        self.devices.write().await.insert(id, device.clone());
        Ok(device)
    }

    async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(self.devices.read().await.values().cloned().collect())
    }

    async fn get_device(&self, id: &str) -> Result<Option<Device>> {
        Ok(self.devices.read().await.get(id).cloned())
    }

    async fn unregister(&self, id: &str) -> Result<()> {
        self.devices.write().await.remove(id);
        Ok(())
    }

    async fn heartbeat(&self, id: &str) -> Result<()> {
        self.heartbeats.lock().unwrap().push(id.to_string());
        Ok(())
    }

    async fn set_discovered_profile(
        &self,
        id: &str,
        device_type: &str,
        capabilities: &[String],
    ) -> Result<()> {
        self.retypes
            .lock()
            .unwrap()
            .push((id.to_string(), device_type.to_string()));
        if let Some(device) = self.devices.write().await.get_mut(id) {
            device.device_type = device_type.to_string();
            device.capabilities = capabilities.to_vec();
        }
        Ok(())
    }
}

/// Connect and run the bridge through its initial sync.
async fn start_adapter(
    url: &str,
) -> (
    Arc<MatterDeviceControl>,
    Arc<MockRegistry>,
    Arc<dyn EventBus>,
    BusStream,
) {
    start_adapter_with(url, Arc::new(MockRegistry::default())).await
}

/// The same, over a pre-filled registry: the only route to `sync_device`'s known-device branch.
async fn start_adapter_with(
    url: &str,
    registry: Arc<MockRegistry>,
) -> (
    Arc<MatterDeviceControl>,
    Arc<MockRegistry>,
    Arc<dyn EventBus>,
    BusStream,
) {
    let (client, events) = MatterClient::connect(url).await.unwrap();
    let bus: Arc<dyn EventBus> = Arc::new(InProcessEventBus::new());
    let received = bus.subscribe();
    let control = Arc::new(MatterDeviceControl::new(client.clone()));

    let registry_dyn: Arc<dyn DeviceRegistry + Send + Sync> = registry.clone();
    let bus_bridge = bus.clone();
    tokio::spawn(async move {
        let _ = run_matter_bridge(
            client,
            events,
            registry_dyn,
            bus_bridge,
            MatterNotifier::disabled(),
            // Fast enough that a test can watch it happen.
            Duration::from_millis(50),
        )
        .await;
    });
    // Let the initial sync land.
    tokio::time::sleep(Duration::from_millis(120)).await;

    (control, registry, bus, received)
}

/// The next bus event, or `None` if nothing arrives promptly.
async fn next_event(stream: &mut BusStream) -> Option<BusEvent> {
    tokio::time::timeout(Duration::from_millis(200), stream.next())
        .await
        .ok()
        .flatten()
}

/// The `control` frames the mock received, in order.
fn control_frames(received: &Received) -> Vec<Value> {
    received
        .lock()
        .unwrap()
        .iter()
        .filter(|f| f["op"] == "control")
        .cloned()
        .collect()
}

// ── Bridge ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_bridge_syncs_the_fabric_into_the_device_registry() {
    let (url, _) = mock_controller(snapshot(vec![light(), sensor()], vec![]), vec![], None).await;
    let (_control, registry, _bus, _rx) = start_adapter(&url).await;

    let devices = registry.list_devices().await.unwrap();
    assert_eq!(devices.len(), 2);
    let kitchen = registry.get_device("matter-2").await.unwrap().unwrap();
    assert_eq!(kitchen.name, "Kitchen Light");
    assert_eq!(kitchen.device_type, "light");
    assert_eq!(kitchen.capabilities, vec!["power", "brightness"]);
}

#[tokio::test]
async fn a_device_nobody_touches_keeps_reading_as_present() {
    // An idle Matter device sends no events, so without a liveness tick it ages offline in 5 min.
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let (_control, registry, _bus, _rx) = start_adapter(&url).await;

    let synced = registry.heartbeats.lock().unwrap().len();

    tokio::time::sleep(Duration::from_millis(180)).await;

    let beats = registry.heartbeats.lock().unwrap();
    assert!(
        beats.len() > synced,
        "an idle device must still be vouched for; heartbeats: {beats:?}"
    );
    assert!(
        beats.iter().all(|id| id == "matter-2"),
        "only the device the controller can see: {beats:?}"
    );
}

#[tokio::test]
async fn a_device_the_controller_has_lost_stops_being_vouched_for_until_it_says_otherwise() {
    // A vouched-for device's `last_seen` never ages, so its card would never turn offline.
    let (url, _) = mock_controller(
        snapshot(vec![light()], vec![]),
        vec![json!({
            "event": "device_availability",
            "payload": { "device_id": "matter-2", "online": false }
        })],
        None,
    )
    .await;
    let (_control, registry, _bus, _rx) = start_adapter(&url).await;

    let after_loss = registry.heartbeats.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(180)).await;

    let beats = registry.heartbeats.lock().unwrap();
    assert_eq!(
        beats.len(),
        after_loss,
        "a device the controller cannot see must not be vouched for: {beats:?}"
    );
}

#[tokio::test]
async fn a_device_the_controller_can_see_again_is_vouched_for_again() {
    // One `online: false` (a subscription lapse, a snapshot before matter.js has a CASE session)
    // must not latch: availability is a level, so a later `true` recovers.
    let (url, _) = mock_controller(
        snapshot(vec![light()], vec![]),
        vec![
            json!({
                "event": "device_availability",
                "payload": { "device_id": "matter-2", "online": false }
            }),
            json!({
                "event": "device_availability",
                "payload": { "device_id": "matter-2", "online": true }
            }),
        ],
        None,
    )
    .await;
    let (_control, registry, _bus, _rx) = start_adapter(&url).await;

    let after_recovery = registry.heartbeats.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(180)).await;

    let beats = registry.heartbeats.lock().unwrap();
    assert!(
        beats.len() > after_recovery,
        "a device the controller can see again must be vouched for again: {beats:?}"
    );
}

#[tokio::test]
async fn a_device_the_snapshot_reports_offline_is_not_given_a_reprieve() {
    // A heartbeat would give it five more minutes of looking present per connect. Registered
    // first: that heartbeat is on the known-device branch.
    let mut absent = light();
    absent["online"] = json!(false);
    let (url, _) = mock_controller(snapshot(vec![absent], vec![]), vec![], None).await;

    let registry = Arc::new(MockRegistry::default());
    registry
        .register(RegisterDeviceRequest {
            id: Some("matter-2".to_string()),
            name: "Kitchen Light".to_string(),
            device_type: "light".to_string(),
            hostname: None,
            capabilities: vec!["power".to_string(), "brightness".to_string()],
            room: None,
        })
        .await
        .unwrap();

    let (_control, registry, _bus, _rx) = start_adapter_with(&url, registry).await;
    tokio::time::sleep(Duration::from_millis(180)).await;

    let beats = registry.heartbeats.lock().unwrap();
    assert!(
        beats.is_empty(),
        "an offline device must not be vouched for at all: {beats:?}"
    );
}

#[tokio::test]
async fn a_sensor_reports_its_current_value_as_soon_as_it_is_synced() {
    // A steady sensor sends no events, so without the snapshot's readings it has none recorded.
    let (url, _) = mock_controller(
        snapshot(vec![sensor()], vec![reading("matter-4", "occupancy", 1.0)]),
        vec![],
        None,
    )
    .await;
    let (_control, _registry, _bus, mut rx) = start_adapter(&url).await;

    let Some(BusEvent::Sensor(reading)) = next_event(&mut rx).await else {
        panic!("the initial value must be published");
    };
    assert_eq!(reading.device_id, "matter-4");
    assert_eq!(reading.sensor_type, "occupancy");
    assert_eq!(reading.value, 1.0);
}

#[tokio::test]
async fn a_reading_event_reaches_the_bus() {
    let (url, _) = mock_controller(
        snapshot(vec![sensor()], vec![]),
        vec![json!({
            "event": "reading",
            "payload": reading("matter-4", "occupancy", 1.0),
        })],
        None,
    )
    .await;
    let (_control, _registry, _bus, mut rx) = start_adapter(&url).await;

    let Some(BusEvent::Sensor(reading)) = next_event(&mut rx).await else {
        panic!("the event must publish");
    };
    assert_eq!(reading.sensor_type, "occupancy");
}

#[tokio::test]
async fn a_device_added_event_registers_the_device() {
    let (url, _) = mock_controller(
        snapshot(vec![], vec![]),
        vec![json!({ "event": "device_added", "payload": { "device": light() } })],
        None,
    )
    .await;
    let (_control, registry, _bus, _rx) = start_adapter(&url).await;

    assert!(registry.get_device("matter-2").await.unwrap().is_some());
}

#[tokio::test]
async fn an_already_registered_device_is_retyped_on_sync() {
    // Otherwise a typing improvement only reaches devices commissioned after it shipped.
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let (client, events) = MatterClient::connect(&url).await.unwrap();

    let registry = Arc::new(MockRegistry::default());
    registry
        .register(RegisterDeviceRequest {
            id: Some("matter-2".to_string()),
            name: "Kitchen Light".to_string(),
            device_type: "matter".to_string(), // the old, untyped registration
            hostname: None,
            capabilities: vec![],
            room: None,
        })
        .await
        .unwrap();

    let registry_dyn: Arc<dyn DeviceRegistry + Send + Sync> = registry.clone();
    let bus: Arc<dyn EventBus> = Arc::new(InProcessEventBus::new());
    tokio::spawn(async move {
        let _ = run_matter_bridge(
            client,
            events,
            registry_dyn,
            bus,
            MatterNotifier::disabled(),
            Duration::from_millis(50),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(120)).await;

    let device = registry.get_device("matter-2").await.unwrap().unwrap();
    assert_eq!(
        device.device_type, "light",
        "the device kept its old typing"
    );
    assert_eq!(device.capabilities, vec!["power", "brightness"]);
    assert_eq!(registry.retypes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unchanged_device_is_not_rewritten_on_every_sync() {
    // Sync runs on every reconnect; an unconditional UPDATE would write every device each time.
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let (_control, registry, _bus, _rx) = start_adapter(&url).await;

    // Registered once with the right profile: nothing to re-derive.
    assert!(registry.retypes.lock().unwrap().is_empty());
}

// ── Control ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_verb_sends_one_control_op_naming_itself() {
    let (url, received) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let (control, _registry, _bus, _rx) = start_adapter(&url).await;

    control.set_power("matter-2", true).await.unwrap();
    control.set_brightness("matter-2", 40).await.unwrap();
    control.set_target_temp("matter-2", 21.5).await.unwrap();
    control.set_locked("matter-2", true).await.unwrap();
    control.set_color("matter-2", 180, 50).await.unwrap();
    control.set_fan_speed("matter-2", 30).await.unwrap();
    control.set_fan_mode("matter-2", "auto").await.unwrap();
    control.set_position("matter-2", 70).await.unwrap();

    let frames = control_frames(&received);
    let verbs: Vec<&str> = frames
        .iter()
        .map(|f| f["params"]["verb"].as_str().unwrap())
        .collect();
    assert_eq!(
        verbs,
        vec![
            "power",
            "brightness",
            "target_temp",
            "locked",
            "color",
            "fan_speed",
            "fan_mode",
            "position"
        ]
    );

    // Values travel as themselves, in GIAP's units, with no conversion here.
    assert_eq!(frames[0]["params"]["value"], json!(true));
    assert_eq!(frames[1]["params"]["value"], json!(40));
    assert_eq!(frames[2]["params"]["value"], json!(21.5));
    assert_eq!(
        frames[4]["params"]["value"],
        json!({"hue": 180, "saturation": 50})
    );
    assert_eq!(frames[6]["params"]["value"], json!("auto"));
    assert!(frames
        .iter()
        .all(|f| f["params"]["device_id"] == "matter-2"));
}

#[tokio::test]
async fn the_outcome_is_what_the_device_did_not_what_was_asked_for() {
    // E.g. a dimmer clamping to its own minimum: echoing the request would claim it obeyed.
    let answer: Answer = Arc::new(
        |_frame| json!({"ok": true, "result": { "applied": { "brightness": 10, "on": true } }}),
    );
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], Some(answer)).await;
    let (control, _registry, _bus, _rx) = start_adapter(&url).await;

    let outcome = control.set_brightness("matter-2", 40).await.unwrap();
    assert_eq!(outcome.device_id, "matter-2");
    assert_eq!(
        outcome.applied.brightness,
        Some(10),
        "the outcome must report the device's value, not the caller's"
    );
    assert_eq!(outcome.applied.on, Some(true));
}

#[tokio::test]
async fn a_refused_command_fails_with_the_controllers_reason() {
    let answer: Answer = Arc::new(|_frame| {
        json!({
            "ok": false,
            "error": {
                "code": "capability_unsupported",
                "message": "Matter device 'matter-2' does not support this capability",
            }
        })
    });
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], Some(answer)).await;
    let (control, _registry, _bus, _rx) = start_adapter(&url).await;

    let error = control.set_position("matter-2", 50).await.unwrap_err();
    assert_eq!(
        crate::client::code_of(&error),
        Some("capability_unsupported")
    );
    assert!(error.to_string().contains("does not support"));
}

#[tokio::test]
async fn control_follows_a_swapped_client() {
    // The supervisor swaps the client in place; a control port built earlier must follow it.
    let (first_url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let (second_url, second_received) =
        mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;

    let (client, _events) = MatterClient::connect(&first_url).await.unwrap();
    let control = Arc::new(MatterDeviceControl::new(client));
    let cell: SharedMatterClient = control.client_handle();

    let (replacement, _events2) = MatterClient::connect(&second_url).await.unwrap();
    *cell.write().await = replacement;

    control.set_power("matter-2", true).await.unwrap();
    assert_eq!(
        control_frames(&second_received).len(),
        1,
        "the command went to the old connection"
    );
}

// ── Reconnect ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_supervisor_reconnects_after_the_connection_drops() {
    let (url, subscribes) = mock_reconnecting_controller(snapshot(vec![light()], vec![])).await;

    let (client, events) = MatterClient::connect(&url).await.unwrap();
    let control = Arc::new(MatterDeviceControl::new(client.clone()));
    let registry: Arc<dyn DeviceRegistry + Send + Sync> = Arc::new(MockRegistry::default());
    let bus: Arc<dyn EventBus> = Arc::new(InProcessEventBus::new());

    tokio::spawn(run_matter_supervisor(
        SupervisorConfig {
            url: url.clone(),
            data_dir: std::path::PathBuf::from("/nonexistent"),
            child: Arc::new(tokio::sync::Mutex::new(None)),
            ble: false,
        },
        control.client_handle(),
        client,
        events,
        registry,
        bus,
        MatterNotifier::disabled(),
    ));

    // First subscribe, drop, backoff (~0.5-1s), reconnect, subscribe again.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        *subscribes.lock().unwrap() >= 2,
        "the fabric must be resynced after a reconnect, saw {} subscribes",
        subscribes.lock().unwrap()
    );
}

#[tokio::test]
async fn a_controller_that_is_not_ours_is_refused_by_name() {
    // Some other WebSocket server, greeting with a frame of its own shape.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/giap", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Text(
            json!({"fabric_id": 1, "schema_version": 11})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        // Hold the connection open so the failure is the greeting, not a drop.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let error = MatterClient::connect(&url).await.err().unwrap();
    assert!(
        error.to_string().contains("Matter controller address"),
        "must tell the user what to fix, got: {error}"
    );
}

#[tokio::test]
async fn a_request_honours_its_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/giap", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(greeting()).await.unwrap();
        while ws.next().await.is_some() {} // read, never answer
    });

    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let error = client
        .send_with_timeout("ping", json!({}), Duration::from_millis(150))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("in time"), "got: {error}");
}

// ── Commissioning ────────────────────────────────────────────────────────────

/// A mock that reports `commissionable` devices and commissions to `device`.
async fn mock_commissioning_controller(commissionable: u32, device: Value) -> (String, Received) {
    let answer: Answer = Arc::new(move |frame: &Value| match frame["op"].as_str() {
        Some("discover") => json!({"ok": true, "result": {"commissionable": commissionable}}),
        Some("commission") => json!({"ok": true, "result": {"device": device}}),
        _ => json!({"ok": true, "result": {}}),
    });
    mock_controller(snapshot(vec![], vec![]), vec![], Some(answer)).await
}

#[tokio::test]
async fn commissioning_with_nothing_in_pairing_mode_says_so_and_says_it_early() {
    let (url, received) = mock_commissioning_controller(0, light()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client, MatterNotifier::disabled());

    let error = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("15 minutes"), "got: {error}");
    let ops: Vec<String> = received
        .lock()
        .unwrap()
        .iter()
        .map(|f| f["op"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !ops.iter().any(|op| op == "commission"),
        "the wait must be skipped, not paid and then explained"
    );
}

#[tokio::test]
async fn a_rejected_setup_code_reaches_the_user_as_one_sentence() {
    let answer: Answer = Arc::new(|frame: &Value| match frame["op"].as_str() {
        Some("discover") => json!({"ok": true, "result": {"commissionable": 1}}),
        Some("commission") => json!({
            "ok": false,
            "error": {"code": "invalid_setup_code", "message": "Invalid pairing code"}
        }),
        _ => json!({"ok": true, "result": {}}),
    });
    let (url, _received) = mock_controller(snapshot(vec![], vec![]), vec![], Some(answer)).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client, MatterNotifier::disabled());

    let error = commissioner
        .commission(
            SetupCode::PairingCode("MT:Y.K9042C00KA0648G00".to_string()),
            None,
        )
        .await
        .unwrap_err();

    // Rendered the way the HTTP route renders it.
    let shown = format!("{error:#}");
    assert_eq!(shown, "Invalid pairing code", "got: {shown}");
}

#[tokio::test]
async fn an_unusable_probe_answer_does_not_block_commissioning() {
    // A probe that itself fails proves nothing, so it never blocks the attempt.
    let answer: Answer = Arc::new(|frame: &Value| match frame["op"].as_str() {
        Some("discover") => json!({"ok": false, "error": {"code": "internal", "message": "no"}}),
        Some("commission") => json!({"ok": true, "result": {"device": light()}}),
        _ => json!({"ok": true, "result": {}}),
    });
    let (url, received) = mock_controller(snapshot(vec![], vec![]), vec![], Some(answer)).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client, MatterNotifier::disabled());

    let device = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap();
    assert_eq!(device.device_id, "matter-2");
    assert!(received
        .lock()
        .unwrap()
        .iter()
        .any(|f| f["op"] == "commission"));
}

#[tokio::test]
async fn commissioning_passes_the_code_and_the_name_to_the_controller() {
    let (url, received) = mock_commissioning_controller(1, light()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client, MatterNotifier::disabled());

    let device = commissioner
        .commission(
            SetupCode::PairingCode("34970112332".to_string()),
            Some("Porch Light".to_string()),
        )
        .await
        .unwrap();

    assert_eq!(device.device_id, "matter-2");
    assert_eq!(
        device.node_id, 2,
        "derived from the id the controller returned"
    );
    assert_eq!(device.name, "Porch Light", "the user's name wins");
    assert_eq!(device.device_type, "light");

    let frame = received
        .lock()
        .unwrap()
        .iter()
        .find(|f| f["op"] == "commission")
        .cloned()
        .unwrap();
    assert_eq!(frame["params"]["code"], "34970112332");
    assert_eq!(frame["params"]["name"], "Porch Light");
}

#[tokio::test]
async fn commissioning_without_a_name_keeps_the_devices_own() {
    let (url, received) = mock_commissioning_controller(1, light()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client, MatterNotifier::disabled());

    let device = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap();
    assert_eq!(device.name, "Kitchen Light");

    let frame = received
        .lock()
        .unwrap()
        .iter()
        .find(|f| f["op"] == "commission")
        .cloned()
        .unwrap();
    assert!(
        frame["params"].get("name").is_none(),
        "no name means none is sent, not an empty one"
    );
}

#[tokio::test]
async fn decommission_names_the_device_by_its_giap_id() {
    let (url, received) = mock_commissioning_controller(1, light()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client, MatterNotifier::disabled());

    commissioner.decommission(18).await.unwrap();

    let frame = received
        .lock()
        .unwrap()
        .iter()
        .find(|f| f["op"] == "decommission")
        .cloned()
        .unwrap();
    assert_eq!(frame["params"]["device_id"], "matter-18");
}

// ── Runtime ──────────────────────────────────────────────────────────────────

async fn wait_for(runtime: &MatterRuntime, want: impl Fn(&MatterState) -> bool) -> MatterStatus {
    for _ in 0..100 {
        let status = runtime.status().await;
        if want(&status.state) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the runtime never reached the wanted state");
}

fn runtime_for() -> Arc<MatterRuntime> {
    MatterRuntime::new(
        std::path::PathBuf::from("/nonexistent"),
        Arc::new(MockRegistry::default()),
        Arc::new(InProcessEventBus::new()),
    )
}

/// Before any `apply` it must not park in `Unreachable`, which startup's wait reads as settled.
#[tokio::test]
async fn a_runtime_that_has_been_asked_for_nothing_does_nothing() {
    let runtime = runtime_for();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let status = runtime.status().await;
    assert!(
        matches!(status.state, MatterState::Disabled),
        "reconciled before being asked: {:?}",
        status.state
    );
    assert!(!status.enabled, "claimed to be enabled before any apply");
    assert!(runtime.commissioner().await.is_none());
}

#[tokio::test]
async fn settle_waits_for_the_apply_that_was_just_made() {
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let runtime = runtime_for();

    runtime.apply(ip_only(url.clone()));
    let settled = runtime.settle(Duration::from_secs(10)).await;

    assert!(
        settled.state.is_connected(),
        "returned before the request it was waiting on landed: {:?}",
        settled.state
    );
}

#[tokio::test]
async fn enabling_connects_and_exposes_a_commissioner() {
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let runtime = runtime_for();

    runtime.apply(ip_only(url.clone()));
    let status = wait_for(&runtime, MatterState::is_connected).await;

    assert!(status.enabled);
    assert_eq!(status.url, url);
    assert!(runtime.commissioner().await.is_some());
}

/// Every settings save touching the address re-applies it; that must not rebuild the fabric.
#[tokio::test]
async fn re_applying_the_same_address_while_connected_does_not_churn() {
    let (url, received) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let runtime = runtime_for();

    runtime.apply(ip_only(url.clone()));
    wait_for(&runtime, MatterState::is_connected).await;
    let subscribes = || {
        received
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["op"] == "subscribe")
            .count()
    };
    let before = subscribes();

    runtime.apply(ip_only(url.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(runtime.status().await.state.is_connected());
    assert_eq!(subscribes(), before, "a no-op save resubscribed the fabric");
}

#[tokio::test]
async fn an_unreachable_controller_reports_the_failure_not_off() {
    let runtime = runtime_for();
    // Unresolvable and non-loopback: fails fast and never triggers a local install.
    runtime.apply(ip_only("ws://controller.invalid:5580/giap".to_string()));

    let status = wait_for(&runtime, |s| matches!(s, MatterState::Unreachable { .. })).await;
    assert!(status.enabled, "unreachable is not the same as off");
}

#[tokio::test]
async fn an_empty_controller_address_is_reported_plainly() {
    // Installs predating the Matter section can be enabled with no address.
    let runtime = runtime_for();
    runtime.apply(ip_only(String::new()));

    let status = wait_for(&runtime, |s| matches!(s, MatterState::Unreachable { .. })).await;
    let MatterState::Unreachable { error } = status.state else {
        panic!("expected unreachable");
    };
    assert!(
        error.contains("no Matter controller address is set"),
        "got: {error}"
    );
}

#[tokio::test]
async fn re_applying_after_a_failure_retries() {
    // Same values while unreachable are a retry: it is what the UI's retry button sends.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("ws://127.0.0.1:{port}/giap");
    drop(listener); // nothing is serving it yet

    let runtime = runtime_for();
    runtime.apply(ip_only(url.clone()));
    wait_for(&runtime, |s| matches!(s, MatterState::Unreachable { .. })).await;

    // Bring a controller up on that exact port, then retry.
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                ws.send(greeting()).await.unwrap();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    let id = frame["id"].as_str().unwrap();
                    ws.send(Message::Text(
                        json!({"id": id, "ok": true, "result": {"devices": [], "readings": []}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            });
        }
    });

    runtime.apply(ip_only(url));
    wait_for(&runtime, MatterState::is_connected).await;
}

#[tokio::test]
async fn shutdown_clears_the_runtime() {
    let (url, _) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let runtime = runtime_for();

    runtime.apply(ip_only(url));
    wait_for(&runtime, MatterState::is_connected).await;

    runtime.shutdown().await;
    assert!(runtime.commissioner().await.is_none());
}

// ── The switchable facade ────────────────────────────────────────────────────

#[derive(Default)]
struct RecordingControl {
    calls: Mutex<Vec<String>>,
}

impl RecordingControl {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn ok(&self, call: String, device_id: &str) -> Result<DeviceControlOutcome> {
        self.calls.lock().unwrap().push(call);
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch::default(),
        ))
    }
}

#[async_trait::async_trait]
impl DeviceControlPort for RecordingControl {
    async fn set_power(&self, id: &str, on: bool) -> Result<DeviceControlOutcome> {
        self.ok(format!("power {id} {on}"), id)
    }
    async fn set_brightness(&self, id: &str, pct: u8) -> Result<DeviceControlOutcome> {
        self.ok(format!("brightness {id} {pct}"), id)
    }
    async fn set_target_temp(&self, id: &str, c: f32) -> Result<DeviceControlOutcome> {
        self.ok(format!("temp {id} {c}"), id)
    }
    async fn set_locked(&self, id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        self.ok(format!("lock {id} {locked}"), id)
    }
}

#[tokio::test]
async fn a_matter_device_is_refused_while_matter_is_off_but_others_fall_back() {
    // The stub reports success for every verb, so a Matter id routed to it would lie.
    let fallback = Arc::new(RecordingControl::default());
    let runtime = runtime_for();
    let control = runtime.device_control(fallback.clone());

    let error = control.set_power("matter-18", true).await.unwrap_err();
    assert!(error.to_string().contains("Matter is off"), "got: {error}");
    assert!(
        fallback.calls().is_empty(),
        "a Matter device must not fall back"
    );

    // Other transports still fall back, which is what the stub is for.
    control.set_power("mqtt-lamp", true).await.unwrap();
    assert_eq!(fallback.calls(), vec!["power mqtt-lamp true"]);
}

#[tokio::test]
async fn a_matter_id_never_falls_back_to_the_stub_however_malformed() {
    // Routing on "parses as a node id" would send any id the grammar can't read to the stub.
    let fallback = Arc::new(RecordingControl::default());
    let runtime = runtime_for();
    let control = runtime.device_control(fallback.clone());

    for device_id in ["matter-90-2", "matter-01", "matter-", "matter-nonsense"] {
        let error = control.set_power(device_id, true).await.unwrap_err();
        assert!(
            error.to_string().contains("Matter is off"),
            "'{device_id}' was not treated as a Matter device: {error}"
        );
    }
    assert!(
        fallback.calls().is_empty(),
        "no Matter id may reach the stub: {:?}",
        fallback.calls()
    );
}

#[tokio::test]
async fn control_switches_to_matter_once_connected() {
    let (url, received) = mock_controller(snapshot(vec![light()], vec![]), vec![], None).await;
    let fallback = Arc::new(RecordingControl::default());
    let runtime = runtime_for();
    let control = runtime.device_control(fallback.clone());

    runtime.apply(ip_only(url));
    wait_for(&runtime, MatterState::is_connected).await;

    control.set_power("matter-2", true).await.unwrap();
    assert_eq!(control_frames(&received).len(), 1);
    assert!(fallback.calls().is_empty());
}

/// The dedupe cache must outlive a connection: re-subscribing replays the same values.
#[tokio::test]
async fn a_reconnect_does_not_republish_a_reading_that_has_not_changed() {
    let (url, _) = mock_controller(
        snapshot(vec![sensor()], vec![reading("matter-3", "occupancy", 1.0)]),
        vec![],
        None,
    )
    .await;

    let registry: Arc<dyn DeviceRegistry + Send + Sync> = Arc::new(MockRegistry::default());
    let bus: Arc<dyn EventBus> = Arc::new(InProcessEventBus::new());
    let mut received = bus.subscribe();

    // The supervisor owns one cache across every reconnect; mirror that here.
    let mut cache = ReadingCache::new();

    for run in 1..=2 {
        let (client, events) = MatterClient::connect(&url).await.unwrap();
        // A run ends when its event stream closes, i.e. a dropped connection.
        let _ = tokio::time::timeout(
            Duration::from_millis(250),
            run_matter_bridge_with_cache(
                client,
                events,
                registry.clone(),
                bus.clone(),
                MatterNotifier::disabled(),
                Duration::from_millis(50),
                &mut cache,
            ),
        )
        .await;
        assert_eq!(cache.len(), 1, "run {run} should leave the reading cached");
    }

    assert!(
        next_event(&mut received).await.is_some(),
        "the first sync must publish the reading"
    );
    assert!(
        next_event(&mut received).await.is_none(),
        "the reconnect republished an unchanged reading, so every rule attached \
         to this sensor fires again with nothing in the house having changed"
    );
}
