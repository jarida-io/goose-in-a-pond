//! What the pond and `pond-server ble-helper` say to each other: one JSON object per line, commands
//! on the helper's stdin, events on its stdout, bytes as standard base64. The helper owns the
//! radio and nothing else; every decision is the pond's.
//!
//! A `link` is one subscription of one central to `tx`, numbered by the helper. Commands name the
//! link they mean, so a late command for a phone that left and came back reaches nothing.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const PROTOCOL_VERSION: u32 = 1;

/// The pond to the helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Advertise the service UUID and this local name, nothing else; replaces any earlier name.
    Advertise {
        local_name: String,
    },
    StopAdvertising,
    /// One `tx` notification to one link, sent in order once CoreBluetooth has room.
    Notify {
        link: u64,
        data: Bytes,
    },
    /// The session on this link is over. CoreBluetooth cannot drop a central, so the helper
    /// refuses its writes and sends it nothing until it unsubscribes.
    Disconnect {
        link: u64,
    },
}

/// The helper to the pond.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// First line, before the radio is touched.
    Ready {
        protocol: u32,
    },
    /// `CBManager.authorization` for this process, read before the manager exists.
    Authorization {
        authorization: Authorization,
    },
    State {
        state: RadioState,
    },
    /// The GATT service is registered; it is gone again whenever the radio leaves `powered_on`.
    ServiceAdded,
    Advertising {
        local_name: String,
    },
    AdvertisingStopped,
    /// A central subscribed to `tx`: a phone has connected. `central` is CoreBluetooth's
    /// identifier for it, for logs only.
    Subscribed {
        link: u64,
        central: String,
        att_mtu: u16,
    },
    Mtu {
        link: u64,
        att_mtu: u16,
    },
    /// One `rx` write, already answered with success.
    Write {
        link: u64,
        data: Bytes,
    },
    /// The central unsubscribed or disconnected.
    Unsubscribed {
        link: u64,
    },
    Error {
        context: String,
        message: String,
    },
}

/// `CBManagerAuthorization`, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    NotDetermined,
    Restricted,
    Denied,
    AllowedAlways,
}

/// `CBManagerState`, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioState {
    Unknown,
    Resetting,
    Unsupported,
    Unauthorized,
    PoweredOff,
    PoweredOn,
}

/// Bytes that travel as standard base64.
#[derive(Clone, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sealed frames, but still nothing a log needs: the length says enough.
        write!(f, "Bytes({} bytes)", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&B64.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        B64.decode(text)
            .map(Bytes)
            .map_err(|_| serde::de::Error::custom("bytes are standard base64"))
    }
}

/// One line, newline included.
pub fn encode_line<T: Serialize>(message: &T) -> serde_json::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn commands_and_events_have_these_wire_shapes() {
        let cases: Vec<(Command, serde_json::Value)> = vec![
            (
                Command::Advertise {
                    local_name: "Pond c1cd".into(),
                },
                json!({"cmd": "advertise", "local_name": "Pond c1cd"}),
            ),
            (Command::StopAdvertising, json!({"cmd": "stop_advertising"})),
            (
                Command::Notify {
                    link: 3,
                    data: Bytes(vec![0, 0, 0, 2, b'{', b'}']),
                },
                json!({"cmd": "notify", "link": 3, "data": "AAAAAnt9"}),
            ),
            (
                Command::Disconnect { link: 3 },
                json!({"cmd": "disconnect", "link": 3}),
            ),
        ];
        for (command, wire) in cases {
            assert_eq!(serde_json::to_value(&command).unwrap(), wire);
            assert_eq!(serde_json::from_value::<Command>(wire).unwrap(), command);
        }

        let events: Vec<(Event, serde_json::Value)> = vec![
            (
                Event::Ready { protocol: 1 },
                json!({"event": "ready", "protocol": 1}),
            ),
            (
                Event::State {
                    state: RadioState::PoweredOn,
                },
                json!({"event": "state", "state": "powered_on"}),
            ),
            (
                Event::Authorization {
                    authorization: Authorization::NotDetermined,
                },
                json!({"event": "authorization", "authorization": "not_determined"}),
            ),
            (Event::ServiceAdded, json!({"event": "service_added"})),
            (
                Event::Subscribed {
                    link: 1,
                    central: "6F1C".into(),
                    att_mtu: 185,
                },
                json!({"event": "subscribed", "link": 1, "central": "6F1C", "att_mtu": 185}),
            ),
            (
                Event::Write {
                    link: 1,
                    data: Bytes(b"hi".to_vec()),
                },
                json!({"event": "write", "link": 1, "data": "aGk="}),
            ),
            (
                Event::Unsubscribed { link: 1 },
                json!({"event": "unsubscribed", "link": 1}),
            ),
            (
                Event::Error {
                    context: "advertise".into(),
                    message: "already advertising".into(),
                },
                json!({"event": "error", "context": "advertise", "message": "already advertising"}),
            ),
        ];
        for (event, wire) in events {
            assert_eq!(serde_json::to_value(&event).unwrap(), wire);
            assert_eq!(serde_json::from_value::<Event>(wire).unwrap(), event);
        }
    }

    #[test]
    fn a_line_ends_in_one_newline_and_bad_base64_is_refused() {
        let line = encode_line(&Command::StopAdvertising).unwrap();
        assert_eq!(line, b"{\"cmd\":\"stop_advertising\"}\n");
        assert!(
            serde_json::from_str::<Command>(r#"{"cmd":"notify","link":1,"data":"%%"}"#).is_err()
        );
        assert_eq!(format!("{:?}", Bytes(vec![1; 40])), "Bytes(40 bytes)");
    }
}
