//! `giap-matter` wire types and pure mappings (spec: `docs/matter-protocol.md`; other side:
//! `matter-server/src/protocol.ts`). Domain-level: never endpoints, clusters or attribute paths.

use crate::client::ControllerCode;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::sensor::SensorReading;
use pond_core::user_data::ports::device_control::{
    DeviceDescription, DeviceState, DeviceStatePatch,
};
use pond_core::user_data::ports::device_registry::Device;
use serde::Deserialize;
use serde_json::{json, Value};

/// Protocol name the greeting must carry.
pub const PROTOCOL_NAME: &str = "giap-matter";

/// Bump only for changes that break an un-updated controller; the client refuses a mismatch.
pub const PROTOCOL_VERSION: u32 = 1;

// ── Greeting ─────────────────────────────────────────────────────────────────

/// The controller speaks first.
#[derive(Debug, Clone, Deserialize)]
pub struct Greeting {
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub fabric_id: Option<u64>,
    #[serde(default)]
    pub matter_js: String,
    /// BLE transport loaded; an older controller without the field has none, so no version bump.
    #[serde(default)]
    pub ble: bool,
}

/// Check a greeting, naming what was found instead, so a wrong address gets an actionable error.
pub fn check_greeting(raw: &str) -> Result<Greeting, String> {
    let greeting: Greeting = serde_json::from_str(raw).map_err(|_| {
        "the controller's greeting was not JSON this version understands".to_string()
    })?;

    if greeting.protocol != PROTOCOL_NAME {
        return Err(format!(
            "expected a {PROTOCOL_NAME} controller but the server at this address identified \
             itself as '{}' — check the Matter controller address",
            if greeting.protocol.is_empty() {
                "something else"
            } else {
                &greeting.protocol
            }
        ));
    }
    if greeting.version != PROTOCOL_VERSION {
        return Err(format!(
            "the controller speaks {PROTOCOL_NAME} v{} but this Pond speaks v{PROTOCOL_VERSION} \
             — the controller and pond-server are from different releases",
            greeting.version
        ));
    }
    Ok(greeting)
}

// ── Frames ───────────────────────────────────────────────────────────────────

/// A parsed frame from the controller.
#[derive(Debug)]
pub enum ServerMessage {
    /// A reply to a request, successful or not.
    Response {
        id: String,
        outcome: Result<Value, WireError>,
    },
    Event {
        event: String,
        payload: Value,
    },
    /// The greeting, or anything else we do not act on.
    Other,
}

/// The controller's structured reason for refusing a request.
#[derive(Debug, Clone, Deserialize)]
pub struct WireError {
    #[serde(default = "internal_code")]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

fn internal_code() -> String {
    "internal".to_string()
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            write!(f, "the controller failed ({})", self.code)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

/// Nothing is advertising for pairing; named because it gets specific user advice.
pub const CODE_NOTHING_PAIRABLE: &str = "no_device_in_pairing_mode";

/// Parse one raw frame.
pub fn parse_server_message(raw: &str) -> ServerMessage {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return ServerMessage::Other;
    };

    if let Some(event) = v.get("event").and_then(Value::as_str) {
        return ServerMessage::Event {
            event: event.to_string(),
            payload: v.get("payload").cloned().unwrap_or(Value::Null),
        };
    }

    if let Some(id) = v.get("id").and_then(Value::as_str) {
        // `ok`, not the `result` key, decides: a failure can carry a null `result`.
        let outcome = if v.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(v
                .get("error")
                .and_then(|e| serde_json::from_value::<WireError>(e.clone()).ok())
                .unwrap_or_else(|| WireError {
                    code: internal_code(),
                    message: "the controller refused the request without saying why".to_string(),
                }))
        };
        return ServerMessage::Response {
            id: id.to_string(),
            outcome,
        };
    }

    ServerMessage::Other
}

/// Build a request frame.
pub fn request_frame(id: &str, op: &str, params: Value) -> String {
    json!({ "id": id, "op": op, "params": params }).to_string()
}

// ── Domain projections ───────────────────────────────────────────────────────

/// A controller-reported device, deliberately without [`Device`]'s rooms or hostnames.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDevice {
    pub id: String,
    pub name: String,
    pub device_type: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Required: a missing field defaulting to `false` would mark every device unreachable.
    pub online: bool,
}

impl WireDevice {
    pub fn to_device(&self) -> Device {
        Device {
            id: self.id.clone(),
            name: self.name.clone(),
            device_type: self.device_type.clone(),
            hostname: None,
            ip_address: None,
            capabilities: self.capabilities.clone(),
            registered_at: Utc::now().to_rfc3339(),
            last_seen: Some(Utc::now().to_rfc3339()),
            is_online: self.online,
            room: None,
        }
    }
}

/// A sensor reading as the controller reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireReading {
    pub device_id: String,
    pub sensor_type: String,
    pub value: f64,
    #[serde(default)]
    pub unit: String,
    /// RFC 3339. The controller's clock, which is this machine's clock.
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

impl WireReading {
    pub fn to_reading(&self) -> SensorReading {
        SensorReading {
            device_id: self.device_id.clone(),
            sensor_type: self.sensor_type.clone(),
            value: self.value,
            unit: self.unit.clone(),
            recorded_at: self.at.unwrap_or_else(Utc::now),
        }
    }
}

/// The `subscribe` result: the whole fabric.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub devices: Vec<WireDevice>,
    #[serde(default)]
    pub readings: Vec<WireReading>,
}

/// The `control` result.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ControlResult {
    #[serde(default)]
    pub applied: DeviceStatePatch,
}

/// The `describe` result, already in GIAP's domain shape.
#[derive(Debug, Clone, Deserialize)]
pub struct DescribeResult {
    pub description: DeviceDescription,
}

/// The `state` result.
#[derive(Debug, Clone, Deserialize)]
pub struct StateResult {
    pub state: DeviceState,
}

/// The `commission` result.
#[derive(Debug, Clone, Deserialize)]
pub struct CommissionResult {
    pub device: WireDevice,
}

/// The `discover` result.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiscoverResult {
    #[serde(default)]
    pub commissionable: u32,
}

/// A `log` event: the controller's own structured record, relayed into `tracing`.
#[derive(Debug, Clone, Deserialize)]
pub struct WireLog {
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
    /// Typed fields, relayed as one string because `tracing` fields must be known at compile time.
    #[serde(default)]
    pub fields: Option<Value>,
}

impl WireLog {
    /// Re-emit into `tracing` at the record's own level, which is why the controller logs NDJSON.
    pub fn relay(&self) {
        let fields = self.rendered_fields();
        let message = if fields.is_empty() {
            redact_setup_code(&self.message)
        } else {
            format!("{} ({fields})", redact_setup_code(&self.message))
        };
        let kind = &self.kind;
        match self.level.as_str() {
            "error" => {
                tracing::error!(target: "giap::trace", kind = %kind, source = "controller", "{message}")
            }
            "warn" => {
                tracing::warn!(target: "giap::trace", kind = %kind, source = "controller", "{message}")
            }
            "info" => {
                tracing::info!(target: "giap::trace", kind = %kind, source = "controller", "{message}")
            }
            _ => tracing::debug!(kind = %kind, source = "controller", "{message}"),
        }
    }

    /// `k=v k=v`, redacted, or empty when there are none.
    pub fn rendered_fields(&self) -> String {
        let Some(Value::Object(map)) = &self.fields else {
            return String::new();
        };
        let rendered = map
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => format!("{k}={s}"),
                other => format!("{k}={other}"),
            })
            .collect::<Vec<_>>()
            .join(" ");
        redact_setup_code(&rendered)
    }
}

/// A `device_availability` event.
#[derive(Debug, Clone, Deserialize)]
pub struct AvailabilityEvent {
    pub device_id: String,
    /// Required, like [`WireDevice::online`]: a default `false` would claim the device is gone.
    pub online: bool,
}

/// A `device_added` / `device_updated` event.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceEvent {
    pub device: WireDevice,
}

/// A `device_removed` event.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceRemovedEvent {
    pub device_id: String,
}

// ── Ids ──────────────────────────────────────────────────────────────────────

// Defined in `pond-core` so `pond-api`'s delete path needn't depend on this adapter.
pub use pond_core::user_data::ports::device_commissioning::{
    is_matter_device_id, matter_bridged_endpoint, matter_device_id, matter_node_id,
};

// ── Redaction ────────────────────────────────────────────────────────────────

/// Matches the controller's own placeholder, so both sides' redactions look the same.
const REDACTED: &str = "[redacted:setup-code]";

/// Strip setup codes (fabric credentials) before a log, error or API response; no `tracing`
/// redactor exists, so call sites apply it. Deliberately over-eager on digits, and idempotent.
pub fn redact_setup_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // QR payloads first, so one isn't redacted piecemeal via its digit runs.
        if chars[i..].starts_with(&['M', 'T', ':']) || chars[i..].starts_with(&['m', 't', ':']) {
            let mut end = i + 3;
            while end < chars.len() && is_qr_char(chars[end]) {
                end += 1;
            }
            out.push_str(REDACTED);
            i = end;
            continue;
        }

        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            let mut end = i;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            // 8 = passcode, 11/21 = manual pairing codes; exact lengths spare node ids and ports.
            if matches!(end - i, 8 | 11 | 21) {
                out.push_str(REDACTED);
                i = end;
                continue;
            }
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_qr_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '$' | '%' | '*' | '+' | '-' | '/' | ':')
}

/// Render the whole cause chain, redacted and minus [`ControllerCode`]; `Display` alone shows
/// only the outermost context.
pub fn describe(error: &anyhow::Error) -> String {
    let prose: Vec<String> = error
        .chain()
        .filter(|frame| frame.downcast_ref::<ControllerCode>().is_none())
        .map(ToString::to_string)
        .collect();

    // No prose at all: name the code rather than saying nothing.
    if prose.is_empty() {
        return format!("{error:#}");
    }
    redact_setup_code(&prose.join(": "))
}

/// Which kind of setup code this is, for logging in place of the value.
pub fn setup_code_kind(code: &str) -> &'static str {
    let trimmed = code.trim();
    if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("MT:") {
        return "pairing_code";
    }
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != trimmed.chars().filter(|c| !matches!(c, ' ' | '-')).count() {
        return "unknown";
    }
    match digits.len() {
        11 | 21 => "pairing_code",
        8 => "passcode",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_round_trip() {
        // The grammar is tested in `pond-core`; this checks the re-export.
        assert_eq!(matter_device_id(18, None), "matter-18");
        assert_eq!(matter_node_id("matter-18"), Some(18));
        assert_eq!(matter_node_id("mqtt-lamp"), None);
        assert_eq!(matter_node_id("matter-not-a-number"), None);
    }

    /// `DescribeResult` maps straight to the domain type, so any field mismatch fails `describe`.
    #[test]
    fn a_description_carries_the_controller_s_vendor_clusters() {
        let result: DescribeResult = serde_json::from_value(serde_json::json!({
            "description": {
                "device_id": "matter-31",
                "device_type": "light",
                "capabilities": [
                    { "verb": "power", "value": { "kind": "boolean" } },
                    { "verb": "brightness", "value": { "kind": "percent" } },
                ],
                "sensors": [],
                "vendor_clusters": [{ "cluster_id": 0xfff1_fc01u32, "endpoint": 1 }],
                "states": [
                    { "name": "door", "value": { "kind": "enum", "values": ["open", "closed"] } },
                ],
            }
        }))
        .expect("a current controller's describe result");

        assert_eq!(result.description.vendor_clusters.len(), 1);
        assert_eq!(
            result.description.vendor_clusters[0].cluster_id,
            0xfff1_fc01
        );
        assert_eq!(result.description.vendor_clusters[0].endpoint, 1);
        assert_eq!(result.description.states.len(), 1);
        assert_eq!(result.description.states[0].name, "door");
    }

    /// The controller in the data dir can be older than this binary.
    #[test]
    fn a_description_without_vendor_clusters_still_reads() {
        let result: DescribeResult = serde_json::from_value(serde_json::json!({
            "description": {
                "device_id": "matter-2",
                "device_type": "light",
                "capabilities": [{ "verb": "power", "value": { "kind": "boolean" } }],
                "sensors": [],
            }
        }))
        .expect("a controller predating vendor clusters");

        assert!(result.description.vendor_clusters.is_empty());
        assert!(result.description.states.is_empty());
        assert_eq!(result.description.capabilities.len(), 1);
    }

    #[test]
    fn responses_are_discriminated_by_ok_not_by_the_result_key() {
        let ok = parse_server_message(r#"{"id":"giap-1","ok":true,"result":{}}"#);
        assert!(matches!(ok, ServerMessage::Response { outcome: Ok(_), .. }));

        let err = parse_server_message(
            r#"{"id":"giap-2","ok":false,"error":{"code":"device_unknown","message":"nope"}}"#,
        );
        let ServerMessage::Response {
            outcome: Err(e), ..
        } = err
        else {
            panic!("expected a failure");
        };
        assert_eq!(e.code, "device_unknown");
        assert_eq!(e.to_string(), "nope");
    }

    #[test]
    fn a_failure_without_an_error_object_still_names_itself() {
        let msg = parse_server_message(r#"{"id":"giap-3","ok":false}"#);
        let ServerMessage::Response {
            outcome: Err(e), ..
        } = msg
        else {
            panic!("expected a failure");
        };
        assert_eq!(e.code, "internal");
        assert!(
            e.to_string().contains("without saying why"),
            "an unexplained failure must still say something"
        );
    }

    #[test]
    fn events_carry_their_payload() {
        let msg = parse_server_message(r#"{"event":"reading","payload":{"value":1}}"#);
        let ServerMessage::Event { event, payload } = msg else {
            panic!("expected an event");
        };
        assert_eq!(event, "reading");
        assert_eq!(payload["value"], 1);
    }

    #[test]
    fn the_greeting_names_a_controller_that_is_not_ours() {
        let ours = check_greeting(
            r#"{"protocol":"giap-matter","version":1,"fabric_id":1,"matter_js":"0.17.9"}"#,
        );
        assert!(ours.is_ok());

        // Another WebSocket server on the configured address.
        let stranger = check_greeting(r#"{"fabric_id":1,"schema_version":11}"#).unwrap_err();
        assert!(
            stranger.contains("Matter controller address"),
            "must tell the user what to fix, got: {stranger}"
        );

        let newer = check_greeting(r#"{"protocol":"giap-matter","version":99}"#).unwrap_err();
        assert!(newer.contains("different releases"), "got: {newer}");
    }

    #[test]
    fn ble_is_read_when_stated_and_absent_means_no() {
        let with_ble = check_greeting(
            r#"{"protocol":"giap-matter","version":1,"fabric_id":1,"matter_js":"0.17.9","ble":true}"#,
        )
        .unwrap();
        assert!(with_ble.ble);

        let older = check_greeting(
            r#"{"protocol":"giap-matter","version":1,"fabric_id":1,"matter_js":"0.17.9"}"#,
        )
        .unwrap();
        assert!(!older.ble, "absent must not read as available");
    }

    #[test]
    fn setup_codes_never_survive_redaction() {
        assert_eq!(
            redact_setup_code("commissioning MT:Y.K9042C00KA0648G00 failed"),
            format!("commissioning {REDACTED} failed")
        );
        assert_eq!(
            redact_setup_code("code 34970112332 rejected"),
            format!("code {REDACTED} rejected")
        );
        assert_eq!(
            redact_setup_code("passcode 20202021 rejected"),
            format!("passcode {REDACTED} rejected")
        );
        assert_eq!(
            redact_setup_code("long 749701123320000000000 x"),
            format!("long {REDACTED} x")
        );
    }

    #[test]
    fn redaction_leaves_ordinary_numbers_alone() {
        assert_eq!(
            redact_setup_code("node 18 on port 5580"),
            "node 18 on port 5580"
        );
        assert_eq!(redact_setup_code("took 1234567 ms"), "took 1234567 ms");
        assert_eq!(redact_setup_code("matter-18"), "matter-18");
    }

    #[test]
    fn redaction_leaves_no_readable_fragment_and_is_idempotent() {
        let once = redact_setup_code("MT:Y.K9042C00KA0648G00");
        assert_eq!(once, REDACTED);
        assert_eq!(redact_setup_code(&once), once, "must be idempotent");

        // The whole payload goes, not the digits inside it.
        assert!(!once.contains("9042"));
    }

    #[test]
    fn code_kind_classifies_without_revealing() {
        assert_eq!(setup_code_kind("MT:Y.K9042C00KA0648G00"), "pairing_code");
        assert_eq!(setup_code_kind("3497-011-2332"), "pairing_code");
        assert_eq!(setup_code_kind("20202021"), "passcode");
        assert_eq!(setup_code_kind("nonsense"), "unknown");
    }

    #[test]
    fn a_relayed_record_carries_its_fields() {
        let record: WireLog = serde_json::from_value(json!({
            "level": "warn",
            "kind": "op_failed",
            "message": "a request failed",
            "fields": { "op": "discover", "error_code": "internal", "duration_ms": 12 },
        }))
        .unwrap();

        let fields = record.rendered_fields();
        assert!(fields.contains("op=discover"), "got: {fields}");
        assert!(fields.contains("error_code=internal"), "got: {fields}");
        assert!(fields.contains("duration_ms=12"), "got: {fields}");
    }

    #[test]
    fn a_relayed_record_without_fields_renders_nothing_extra() {
        let record: WireLog =
            serde_json::from_value(json!({ "level": "info", "kind": "ready", "message": "up" }))
                .unwrap();
        assert_eq!(record.rendered_fields(), "");
    }

    #[test]
    fn relayed_fields_are_redacted_too() {
        let record: WireLog = serde_json::from_value(json!({
            "level": "warn",
            "kind": "op_failed",
            "message": "a request failed",
            "fields": { "error": "PASE failed for MT:Y.K9042C00KA0648G00" },
        }))
        .unwrap();
        assert!(!record.rendered_fields().contains("MT:"));
    }

    #[test]
    fn an_error_is_described_by_its_whole_chain() {
        let error = anyhow::anyhow!("HTTP error: 404 Not Found")
            .context("connecting to the Matter controller at ws://127.0.0.1:5580/giap");

        let described = describe(&error);
        assert!(
            described.contains("404"),
            "the reason was dropped: {described}"
        );
        assert!(
            described.contains("connecting to"),
            "the attempt was dropped"
        );
    }

    #[test]
    fn a_described_error_is_still_redacted() {
        let error = anyhow::anyhow!("PASE failed for MT:Y.K9042C00KA0648G00")
            .context("commissioning failed");
        let described = describe(&error);
        assert!(
            !described.contains("MT:"),
            "leaked a setup code: {described}"
        );
    }

    #[test]
    fn the_wire_device_becomes_a_giap_device() {
        let wire: WireDevice = serde_json::from_value(json!({
            "id": "matter-18",
            "name": "Living Room Fan",
            "device_type": "fan",
            "capabilities": ["power", "fan_speed"],
            "online": true,
        }))
        .unwrap();

        let device = wire.to_device();
        assert_eq!(device.id, "matter-18");
        assert_eq!(device.device_type, "fan");
        assert_eq!(device.capabilities, vec!["power", "fan_speed"]);
        assert!(device.is_online);
        // The controller has no opinion on these, so nothing is invented.
        assert!(device.room.is_none());
        assert!(device.hostname.is_none());
    }

    #[test]
    fn the_applied_patch_deserialises_straight_into_the_core_type() {
        let result: ControlResult =
            serde_json::from_value(json!({ "applied": { "on": true, "brightness": 40 } })).unwrap();
        assert_eq!(result.applied.on, Some(true));
        assert_eq!(result.applied.brightness, Some(40));
        assert_eq!(result.applied.position, None);
    }
}
