//! Thin WebSocket client for the Matter controller.
//!
//! One connection carries both request/response pairs (matched by `id`) and
//! unsolicited events. A background read task routes responses to their waiting
//! callers and fans events out on an mpsc channel the bridge consumes.
//!
//! It also relays the controller's own `log` events into `tracing`. The Rust
//! side pipes the child's stderr as well, so a controller GIAP started is
//! audible twice over; this path is what makes a controller the operator runs
//! themselves — where there is no pipe to read — just as legible.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{
    check_greeting, parse_server_message, redact_setup_code, request_frame, ServerMessage,
    WireError, WireLog,
};

/// How long an op may wait for its response. Device commands round-trip in tens
/// of milliseconds; this bounds a wedged controller.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// An unsolicited event from the controller.
#[derive(Debug, Clone)]
pub struct MatterEvent {
    pub event: String,
    pub payload: Value,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>>;

pub struct MatterClient {
    tx: mpsc::Sender<Message>,
    pending: Pending,
    next_id: AtomicU64,
    /// Whether this controller has a BLE transport, as its greeting said.
    ///
    /// Kept because the commissioning pre-flight has to reason about it: that
    /// probe is an mDNS browse, and a device out of its box advertises over BLE
    /// and not on mDNS at all. See `refuse_when_nothing_is_pairable`.
    ble: bool,
}

impl MatterClient {
    /// Connect, check the greeting, and start the read/write tasks. Returns the
    /// client plus the stream of unsolicited events.
    ///
    /// Relays the controller's `log` events into `tracing`. Use
    /// [`connect_to_managed`] instead when GIAP spawned the controller itself
    /// and is already relaying its stderr.
    pub async fn connect(url: &str) -> Result<(Arc<Self>, mpsc::Receiver<MatterEvent>)> {
        Self::open(url, true).await
    }

    /// Connect to a controller whose stderr GIAP is already relaying, so its
    /// `log` events are dropped rather than logged a second time.
    pub async fn connect_to_managed(url: &str) -> Result<(Arc<Self>, mpsc::Receiver<MatterEvent>)> {
        Self::open(url, false).await
    }

    async fn open(url: &str, relay_logs: bool) -> Result<(Arc<Self>, mpsc::Receiver<MatterEvent>)> {
        let (socket, _) = connect_async(url)
            .await
            .with_context(|| format!("connecting to the Matter controller at {url}"))?;
        let (mut sink, mut stream) = socket.split();

        // Handshake: the controller greets immediately, and the greeting says
        // whether it is one this version can talk to at all.
        let frame = tokio::time::timeout(COMMAND_TIMEOUT, stream.next())
            .await
            .context("timed out waiting for the controller's greeting")?
            .ok_or_else(|| anyhow!("the controller closed the connection during the handshake"))?
            .context("the controller's handshake failed")?;
        let greeting =
            check_greeting(frame.to_text().unwrap_or_default()).map_err(|e| anyhow!(e))?;

        tracing::info!(
            target: "giap::trace",
            kind = "matter_connected",
            url,
            fabric_id = greeting.fabric_id,
            matter_js = %greeting.matter_js,
            ble = greeting.ble,
            "connected to the Matter controller"
        );

        let (out_tx, mut out_rx) = mpsc::channel::<Message>(32);
        let (event_tx, event_rx) = mpsc::channel::<MatterEvent>(64);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        // Writer task: serialises all outbound frames.
        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
        });

        // Reader task: routes responses to waiters, events to the bridge.
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                let Ok(Message::Text(text)) = frame else {
                    if frame.is_err() {
                        break;
                    }
                    continue; // pings etc.
                };
                match parse_server_message(&text) {
                    ServerMessage::Response { id, outcome } => {
                        let Some(waiter) = pending_reader
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&id)
                        else {
                            continue; // timed out already; the caller has gone
                        };
                        let _ = waiter.send(outcome.map_err(controller_error));
                    }
                    ServerMessage::Event { event, payload } => {
                        if event == "log" {
                            // Only when nothing else is: for a controller GIAP
                            // spawned, its stderr is already piped and relayed,
                            // and doing both put every controller line in the
                            // log twice.
                            if relay_logs {
                                if let Ok(record) = serde_json::from_value::<WireLog>(payload) {
                                    record.relay();
                                }
                            }
                            continue;
                        }
                        // Full channel = slow bridge; dropping is fine for state
                        // updates, where the last write wins downstream.
                        let _ = event_tx.try_send(MatterEvent { event, payload });
                    }
                    ServerMessage::Other => {}
                }
            }
            // Connection gone: fail all in-flight ops so callers see it.
            let mut map = pending_reader.lock().unwrap_or_else(|e| e.into_inner());
            for (_, waiter) in map.drain() {
                let _ = waiter.send(Err(anyhow!("the Matter controller connection was lost")));
            }
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_disconnected",
                "the Matter controller connection closed"
            );
        });

        Ok((
            Arc::new(Self {
                tx: out_tx,
                pending,
                next_id: AtomicU64::new(1),
                ble: greeting.ble,
            }),
            event_rx,
        ))
    }

    /// Does this controller pair over Bluetooth as well as over IP?
    ///
    /// What the controller actually loaded, not what was asked for: a request
    /// that could not be honoured reads as `false` here, which is the truth
    /// about what it can reach.
    pub fn has_ble(&self) -> bool {
        self.ble
    }

    /// Send `op` and await its response, using the default timeout.
    pub async fn send(&self, op: &str, params: Value) -> Result<Value> {
        self.send_with_timeout(op, params, COMMAND_TIMEOUT).await
    }

    /// Send `op` with an explicit timeout. Commissioning needs this: pairing a
    /// device onto the fabric routinely outlasts [`COMMAND_TIMEOUT`], and cutting
    /// it short would abandon a half-commissioned node.
    pub async fn send_with_timeout(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = format!("giap-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);

        let frame = Message::Text(request_frame(&id, op, params).into());
        if self.tx.send(frame).await.is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(anyhow!("the Matter controller connection was lost"));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!(
                "the Matter controller dropped the response channel"
            )),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(anyhow!(
                    "the Matter controller did not answer '{op}' in time"
                ))
            }
        }
    }
}

/// Carry the controller's error code alongside its message.
///
/// The code is what callers key their user-facing advice off — "nothing is in
/// pairing mode" needs a different sentence from "that device is unreachable" —
/// so it is preserved in the chain rather than flattened into prose. Redacted
/// here as well as on the controller: this is the last point before the message
/// reaches a log, an API response, or the model.
fn controller_error(error: WireError) -> anyhow::Error {
    // The code is the SOURCE and the message the context, not the other way
    // round: `Display` is what the user and the model read, so it has to be the
    // controller's prose. Attaching the message as context over a code-shaped
    // error gives both — a readable error, with the code still reachable by
    // `downcast_ref` for the callers that branch on it.
    anyhow::Error::new(ControllerCode(error.code)).context(redact_setup_code(&error.message))
}

/// The controller's `error.code`, attached to the error chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCode(pub String);

impl std::fmt::Display for ControllerCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ControllerCode {}

/// The controller's error code for `error`, if it carries one.
///
/// `downcast_ref` on the `anyhow::Error` itself, not a walk over `chain()`:
/// anyhow stores a context frame as its own wrapper type, so the chain yields
/// that wrapper rather than the `ControllerCode` inside it, and iterating finds
/// nothing. The downcast is the supported way to reach a context value, and it
/// keeps working after further `.context()` calls further up the stack.
pub fn code_of(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<ControllerCode>()
        .map(|code| code.0.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CODE_NOTHING_PAIRABLE;

    #[test]
    fn the_controllers_error_code_survives_into_the_error_chain() {
        // The code is what commissioning keys its "put the device into pairing
        // mode" advice off, so flattening it into prose would put that advice
        // back onto substring-matching against controller wording.
        let error = controller_error(WireError {
            code: CODE_NOTHING_PAIRABLE.to_string(),
            message: "nothing is advertising".to_string(),
        });

        assert_eq!(code_of(&error), Some(CODE_NOTHING_PAIRABLE));
        assert!(error.to_string().contains("nothing is advertising"));
    }

    #[test]
    fn a_controller_error_is_redacted_before_it_becomes_an_error() {
        // matter.js and CHIP echo what they were given, so a failed commission is
        // the most likely way a setup code would ever reach a log.
        let error = controller_error(WireError {
            code: "commission_failed".to_string(),
            message: "PASE failed for MT:Y.K9042C00KA0648G00".to_string(),
        });

        let rendered = error.to_string();
        assert!(!rendered.contains("MT:"), "leaked a setup code: {rendered}");
        assert!(rendered.contains("[redacted:setup-code]"));
    }

    #[test]
    fn an_error_without_a_code_reports_none() {
        assert_eq!(code_of(&anyhow!("plain failure")), None);
    }
}
