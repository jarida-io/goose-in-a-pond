//! The setup channel's messages (`pond-setup-channel.md`, "Messages"): JSON in the HTTPS API's
//! shapes. Every request carries `id` and `type`; every response echoes both, with `ok`.

use std::borrow::Cow;

use pond_core::network::domain::wifi::WifiPassphrase;
use serde::Deserialize;
use serde_json::{json, Map, Value};

pub const PROTOCOL_VERSION: u32 = 1;

/// Channel-level refusals the HTTPS API has no route for; every other code is the API's own.
pub mod codes {
    /// A sealed-phase request in the clear, or a clear-phase one after the exchange.
    pub const WRONG_PHASE: &str = "wrong_phase";
    pub const UNKNOWN_TYPE: &str = "unknown_type";
    /// The type's fields are missing or malformed.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// The connection's `pake_init` allowance is spent; the session ends after this answer.
    pub const TOO_MANY_ATTEMPTS: &str = "too_many_attempts";
    pub const INTERNAL_ERROR: &str = "internal_error";
    /// As `POST /handshake/pake/init` answers a credential it does not know.
    pub const INVALID_CREDENTIAL: &str = "invalid_credential";
    /// As `POST /handshake/pake/verify` answers a challenge it never issued.
    pub const UNKNOWN_CHALLENGE: &str = "unknown_challenge";
    /// The host has no Wi-Fi station at all.
    pub const UNSUPPORTED: &str = "unsupported";
}

/// Which half of a session a request belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before the exchange, in the clear.
    Clear,
    /// After `pake_verify`, inside a sealed envelope.
    Sealed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestType {
    Hello,
    SpokenCode,
    PakeInit,
    PakeVerify,
    Status,
    WifiScan,
    WifiJoin,
}

impl RequestType {
    pub const ALL: [Self; 7] = [
        Self::Hello,
        Self::SpokenCode,
        Self::PakeInit,
        Self::PakeVerify,
        Self::Status,
        Self::WifiScan,
        Self::WifiJoin,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::SpokenCode => "spoken_code",
            Self::PakeInit => "pake_init",
            Self::PakeVerify => "pake_verify",
            Self::Status => "status",
            Self::WifiScan => "wifi_scan",
            Self::WifiJoin => "wifi_join",
        }
    }

    pub fn phase(self) -> Phase {
        match self {
            Self::Hello | Self::SpokenCode | Self::PakeInit | Self::PakeVerify => Phase::Clear,
            Self::Status | Self::WifiScan | Self::WifiJoin => Phase::Sealed,
        }
    }
}

/// The two fields every request carries. A message without them is unreadable.
#[derive(Debug, Deserialize)]
pub struct Header<'a> {
    pub id: u64,
    #[serde(rename = "type", borrow)]
    pub kind: Cow<'a, str>,
}

/// `None` unless the message is a JSON object: serde would also read a struct from an array.
pub fn parse_header(message: &[u8]) -> Option<Header<'_>> {
    let first = message.iter().find(|byte| !byte.is_ascii_whitespace())?;
    if *first != b'{' {
        return None;
    }
    serde_json::from_slice(message).ok()
}

/// `wifi_join`'s body. The passphrase is omitted for an open network.
#[derive(Debug, Deserialize)]
pub struct WifiJoinRequest {
    pub ssid: String,
    #[serde(default)]
    pub passphrase: Option<WifiPassphrase>,
    #[serde(default)]
    pub hidden: bool,
}

/// `{id, type, ok: true, ...fields}`. `fields` is an object; anything else adds nothing.
pub fn ok(id: u64, kind: RequestType, fields: Value) -> Value {
    let mut body = envelope_fields(id, kind.as_str(), true);
    if let Value::Object(fields) = fields {
        body.extend(fields);
    }
    Value::Object(body)
}

/// `{id, type, ok: false, error}`, and `retry_after` in whole seconds where waiting helps.
pub fn refused(id: u64, kind: &str, code: &str, retry_after_secs: Option<u64>) -> Value {
    let mut body = envelope_fields(id, kind, false);
    body.insert("error".into(), json!(code));
    if let Some(secs) = retry_after_secs {
        body.insert("retry_after".into(), json!(secs));
    }
    Value::Object(body)
}

fn envelope_fields(id: u64, kind: &str, ok: bool) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("id".into(), json!(id));
    body.insert("type".into(), json!(kind));
    body.insert("ok".into(), json!(ok));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_round_trips_its_wire_name_and_knows_its_phase() {
        for kind in RequestType::ALL {
            assert_eq!(RequestType::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(RequestType::parse("Hello"), None);
        assert_eq!(RequestType::parse("speak_code"), None);
        let sealed: Vec<&str> = RequestType::ALL
            .into_iter()
            .filter(|k| k.phase() == Phase::Sealed)
            .map(RequestType::as_str)
            .collect();
        assert_eq!(sealed, ["status", "wifi_scan", "wifi_join"]);
    }

    #[test]
    fn a_header_needs_an_integer_id_and_a_string_type() {
        let header = parse_header(br#"{"id": 7, "type": "hello", "extra": [1]}"#).unwrap();
        assert_eq!((header.id, header.kind.as_ref()), (7, "hello"));
        for unreadable in [
            &br#"{"type": "hello"}"#[..],
            br#"{"id": 7}"#,
            br#"{"id": -1, "type": "hello"}"#,
            br#"{"id": "7", "type": "hello"}"#,
            br#"{"id": 7, "type": 3}"#,
            br#"[7, "hello"]"#,
            b"not json",
            b"",
        ] {
            assert!(parse_header(unreadable).is_none(), "{unreadable:?}");
        }
    }

    #[test]
    fn answers_echo_the_id_and_type() {
        assert_eq!(
            ok(3, RequestType::Hello, json!({"protocol": 1})),
            json!({"id": 3, "type": "hello", "ok": true, "protocol": 1})
        );
        assert_eq!(
            refused(4, "pake_init", "setup_code_cooling", Some(30)),
            json!({"id": 4, "type": "pake_init", "ok": false,
                   "error": "setup_code_cooling", "retry_after": 30})
        );
        assert_eq!(
            refused(5, "frobnicate", codes::UNKNOWN_TYPE, None),
            json!({"id": 5, "type": "frobnicate", "ok": false, "error": "unknown_type"})
        );
    }

    #[test]
    fn a_join_request_keeps_its_passphrase_out_of_debug() {
        let join: WifiJoinRequest = serde_json::from_slice(
            br#"{"id": 9, "type": "wifi_join", "ssid": "Home", "passphrase": "hunter2hunter2"}"#,
        )
        .unwrap();
        assert_eq!(join.ssid, "Home");
        assert!(!join.hidden);
        assert!(!format!("{join:?}").contains("hunter2"));
        let open: WifiJoinRequest =
            serde_json::from_slice(br#"{"ssid": "Cafe", "hidden": true}"#).unwrap();
        assert!(open.passphrase.is_none() && open.hidden);
    }
}
