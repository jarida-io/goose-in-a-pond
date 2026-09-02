//! The `giap-matter` wire protocol — types and pure functions, so every mapping
//! is unit-testable without a WebSocket.
//!
//! `docs/matter-protocol.md` is the specification; `matter-server/src/protocol.ts`
//! is the other implementation. The protocol is domain-level on purpose: it
//! carries devices, readings and control verbs, and never endpoints, clusters or
//! attribute paths. All the Matter vocabulary lives in the controller, which has
//! matter.js's typed cluster models to do it with — so nothing in this crate has
//! to know what a cluster is.
//!
//! ```text
//! → {"id": "giap-1", "op": "control", "params": {…}}
//! ← {"id": "giap-1", "ok": true,  "result": {…}}
//! ← {"id": "giap-1", "ok": false, "error": {"code": "…", "message": "…"}}
//! ← {"event": "reading", "payload": {…}}
//! ```

use crate::client::ControllerCode;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::sensor::SensorReading;
use pond_core::user_data::ports::device_control::{
    DeviceDescription, DeviceState, DeviceStatePatch,
};
use pond_core::user_data::ports::device_registry::Device;
use serde::Deserialize;
use serde_json::{json, Value};

/// Identifies the protocol in the greeting. A controller that does not say this
/// is not one this crate can talk to.
pub const PROTOCOL_NAME: &str = "giap-matter";

/// Bumped when a change would break a controller that has not been updated with
/// it. The client refuses a mismatch rather than guessing.
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
    /// Whether the controller loaded a BLE transport, so a device that has
    /// never been on the network can be paired.
    ///
    /// `default` means a controller predating the field reads as "no BLE",
    /// which is what such a controller has — so no `PROTOCOL_VERSION` bump is
    /// owed, by that rule's own terms.
    #[serde(default)]
    pub ble: bool,
}

/// Check a greeting frame, naming what was found when it is not ours.
///
/// The failure this exists for is an address pointing at a server that is not
/// this controller: without the check the first `subscribe` fails somewhere
/// inside serde with a message about an unexpected field, which tells the user
/// nothing they can act on. A server reachable on the right path and speaking
/// the wrong protocol is exactly the case the name and version are for.
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

/// The controller could not find anything advertising itself for pairing. Named
/// because the user-facing advice for it is specific and actionable.
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
        // `ok` is the discriminant rather than the presence of a `result` key: a
        // successful op with no result is `{"ok": true, "result": {}}`, and
        // keying off `result` would read a failure with a null result as one.
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

/// A device as the controller reports it. Deliberately smaller than GIAP's own
/// [`Device`]: the controller knows nothing about rooms, hostnames or when a
/// device was first registered, and inventing values for those here is what
/// would make a Matter device look different from every other kind.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDevice {
    pub id: String,
    pub name: String,
    pub device_type: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Required, unlike `capabilities`. `bool::default()` is `false`, so a
    /// defaulted `online` means "offline" — and a field the controller stopped
    /// sending, or renamed, would mark every device on the fabric unreachable
    /// with no error raised anywhere. An absent capability list is a device with
    /// nothing to drive, which is a real thing; an absent reachability is not.
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

/// The `subscribe` result: the whole fabric, so a fresh connection knows it
/// without waiting for anything to change.
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

/// The `describe` result. The description's own shape is GIAP's, so it
/// deserialises straight into the domain type with no mapping step.
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
    /// The record's typed fields. Relayed as one rendered string rather than as
    /// `tracing` fields, which have to be known at compile time — dropping them
    /// entirely turned "a request failed" into the whole account of a failure
    /// whose op, error code and reason the controller had all supplied.
    #[serde(default)]
    pub fields: Option<Value>,
}

impl WireLog {
    /// Re-emit this record into `tracing` at the level it names.
    ///
    /// The whole reason the controller logs NDJSON rather than prose: a relay
    /// that cannot tell an error from a debug line has to flatten everything to
    /// one level, and a controller whose failures arrive at `debug` is most of
    /// the way back to being silent.
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
    /// Required, for the reason on [`WireDevice::online`]: the whole payload of
    /// this event is one boolean, and defaulting it to `false` turns a malformed
    /// frame into a confident claim that the device is gone.
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

// One grammar, one definition. This crate had its own copy of
// `matter-<node_id>` parsing and `pond-core` had another, in a different crate,
// with nothing tying them together and separate tests for each. Two
// implementations of one string format is a defect waiting for the format to
// change — and it is about to, since a bridged device needs an endpoint in its id.
//
// `pond-core` keeps the definition because the generic delete path in `pond-api`
// needs it and must not depend on this adapter. This crate already depends on
// `pond-core`, so the duplicate goes and the names stay where callers expect them.
pub use pond_core::user_data::ports::device_commissioning::{
    is_matter_device_id, matter_bridged_endpoint, matter_device_id, matter_node_id,
};

// ── Redaction ────────────────────────────────────────────────────────────────

/// What a setup code is replaced with. Matches the controller's own placeholder,
/// so a redacted string looks the same whichever side redacted it.
const REDACTED: &str = "[redacted:setup-code]";

/// Strip Matter setup codes out of anything on its way to a log line, an error
/// message, or the API.
///
/// A pairing code grants fabric access: it is a credential, and one in
/// `pond.log.<date>` or in `pond_logs.db` is a working credential for anyone who
/// reads the file. There is no `Redactor` on the tracing pipeline — the
/// `RedactingEventLog` decorator covers the durable event log and egress, not
/// `tracing` — so this is applied at the call site, in the same spirit as
/// `wolfram.rs`'s `redact_appid`.
///
/// It matters most for errors, which are the strings nobody writes deliberately:
/// matter.js and the CHIP layer beneath it echo what they were given, and
/// `MatterState::Unreachable { error }` is **served over HTTP** by
/// `GET /api/v1/matter/status`.
///
/// Deliberately over-eager on the digit forms: redacting a run that happened not
/// to be a code costs a vaguer log line, while missing one writes a credential to
/// disk. Idempotent, so a string already redacted by the controller is unchanged.
pub fn redact_setup_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // QR payloads first, so a digit run inside one cannot be redacted
        // piecemeal leaving the rest of the payload readable.
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
            // 8 is a passcode, 11 and 21 the manual pairing code forms. Bounded
            // on both sides so a node id, a port or a timestamp is left alone.
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

/// Render an error for a human: the whole cause chain, redacted, prose only.
///
/// `anyhow::Error`'s plain `Display` prints only the OUTERMOST context, so
/// `error = %e` on a failure like "connecting to the controller at ws://…"
/// showed the attempt and threw away the reason — which is the one thing the
/// reader needs. Walking the chain fixes that, and this is the only way any
/// error in this crate should reach a log, an API response, or the model.
///
/// The [`ControllerCode`] frame is skipped, because it is not prose. It sits at
/// the bottom of the chain deliberately — the code is the SOURCE and the message
/// the context, so [`code_of`](crate::code_of) can still reach it — but `{:#}`
/// renders every frame, which is how a rejected setup code reached the user as
/// "commissioning failed: Invalid pairing code: commission_failed". Two of those
/// three fragments were bookkeeping. Callers that want the code ask for it by
/// name and put it in a field of its own.
pub fn describe(error: &anyhow::Error) -> String {
    let prose: Vec<String> = error
        .chain()
        .filter(|frame| frame.downcast_ref::<ControllerCode>().is_none())
        .map(ToString::to_string)
        .collect();

    // A code with no message at all: say which code rather than saying nothing.
    // `WireError`'s own `Display` makes the same choice for the same reason.
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
        // The grammar itself is tested where it is defined, in `pond-core`. This
        // asserts the re-export reaches this crate, since every call site here
        // imports it from `protocol`.
        assert_eq!(matter_device_id(18, None), "matter-18");
        assert_eq!(matter_node_id("matter-18"), Some(18));
        assert_eq!(matter_node_id("mqtt-lamp"), None);
        assert_eq!(matter_node_id("matter-not-a-number"), None);
    }

    /// The one wire contract nothing checked. `DescribeResult` deserialises straight
    /// into the domain type with no mapping step, so a field the controller adds and
    /// the domain type has not got fails the whole call — the device is then reported
    /// as one that cannot describe itself, naming nothing that would lead to the cause.
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

    /// The controller lives in the data dir and can be older than the binary reading
    /// it, so an absent field has to mean "none" rather than failing the description.
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
        // A successful op with an empty result must not read as a failure, and a
        // failure whose result is null must not read as a success.
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

        // Some other WebSocket server on the configured address, greeting with
        // a frame of its own shape.
        let stranger = check_greeting(r#"{"fabric_id":1,"schema_version":11}"#).unwrap_err();
        assert!(
            stranger.contains("Matter controller address"),
            "must tell the user what to fix, got: {stranger}"
        );

        let newer = check_greeting(r#"{"protocol":"giap-matter","version":99}"#).unwrap_err();
        assert!(newer.contains("different releases"), "got: {newer}");
    }

    /// A controller predating the field reads as "no BLE", which is what such a
    /// controller has — so the field earns no `PROTOCOL_VERSION` bump, by that
    /// rule's own terms, and neither side breaks against the other.
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
        // Over-eager on length would blank node ids, ports and durations, and a
        // log that redacts everything is as useless as one that redacts nothing.
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
        // The bug this exists for: the relay read level, kind and message and
        // dropped `fields`, so a controller that had reported the op, the error
        // code and the reason arrived in the log as "a request failed".
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
        // Fields are the likeliest place for a code to travel, since that is
        // where structured values go.
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
        // The bug this exists for: the adapter reported "connecting to the
        // Matter controller at ws://127.0.0.1:5580/giap" and nothing else, so a
        // controller answering 404 to the handshake and one refusing the
        // connection outright were the same sentence.
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
        // The wire names its fields exactly as `DeviceStatePatch` does, which is
        // what lets the control port report what the DEVICE did rather than what
        // the caller asked for.
        let result: ControlResult =
            serde_json::from_value(json!({ "applied": { "on": true, "brightness": 40 } })).unwrap();
        assert_eq!(result.applied.on, Some(true));
        assert_eq!(result.applied.brightness, Some(40));
        assert_eq!(result.applied.position, None);
    }
}
