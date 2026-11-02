//! `GET /devices/events`: device state changes, numbered so a phone that loses the stream resumes
//! after the last one it saw instead of reading every device again.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
use serde_json::{json, Value};
use tokio::sync::broadcast;

/// How far back a phone can resume; one further behind is told to read everything again.
const KEEP: usize = 512;

/// Below [`KEEP`], so whatever a lagging subscriber misses is still in the ring.
const LIVE: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub seq: u64,
    pub data: Value,
}

/// Where a stream starts.
pub enum Since {
    /// What the phone missed, all still kept.
    Frames(Vec<Frame>),
    /// The phone is further behind than the ring, or its number is from before a restart.
    Resync,
}

pub struct DeviceEvents {
    ring: Mutex<Ring>,
    live: broadcast::Sender<Frame>,
}

struct Ring {
    /// The number this boot started from: the clock in microseconds, so a number from before a
    /// restart is always below it and never mistaken for one from this boot.
    first: u64,
    next: u64,
    frames: VecDeque<Frame>,
}

impl Default for DeviceEvents {
    fn default() -> Self {
        let first = chrono::Utc::now().timestamp_micros().max(1) as u64;
        Self::starting_at(first)
    }
}

impl DeviceEvents {
    fn starting_at(first: u64) -> Self {
        Self {
            ring: Mutex::new(Ring {
                first,
                next: first,
                frames: VecDeque::new(),
            }),
            live: broadcast::channel(LIVE).0,
        }
    }

    /// Number a state change, keep it, and send it to every open stream.
    pub fn push(&self, change: &DeviceStateChanged) -> u64 {
        let mut ring = self.ring.lock().unwrap_or_else(|p| p.into_inner());
        let seq = ring.next;
        ring.next += 1;
        let frame = Frame {
            seq,
            data: json!({
                "seq": seq,
                "device_id": change.device_id.as_str(),
                "key": change.key,
                "value": plain(&change.value),
                "changed_at": change.changed_at.to_rfc3339(),
            }),
        };
        if ring.frames.len() == KEEP {
            ring.frames.pop_front();
        }
        ring.frames.push_back(frame.clone());
        // Sent under the lock, so a stream opening now gets each frame once: kept or live.
        let _ = self.live.send(frame);
        seq
    }

    /// What follows `after` (nothing without one), and the live frames from then on.
    pub fn open(&self, after: Option<u64>) -> (Since, broadcast::Receiver<Frame>) {
        let ring = self.ring.lock().unwrap_or_else(|p| p.into_inner());
        let live = self.live.subscribe();
        let Some(after) = after else {
            return (Since::Frames(Vec::new()), live);
        };
        let oldest = ring.frames.front().map_or(ring.next, |frame| frame.seq);
        let since = if after + 1 < ring.first || after + 1 < oldest || after >= ring.next {
            Since::Resync
        } else {
            Since::Frames(
                ring.frames
                    .iter()
                    .filter(|frame| frame.seq > after)
                    .cloned()
                    .collect(),
            )
        };
        (since, live)
    }

    /// Keep every device state change the bus carries; runs until the bus closes.
    pub fn follow(self: Arc<Self>, bus: Arc<dyn EventBus>) -> tokio::task::JoinHandle<()> {
        let mut events = bus.subscribe();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if let BusEvent::Device(change) = event {
                    self.push(&change);
                }
            }
        })
    }
}

/// The value as the app reads it: a boolean, a number or text, not serde's tagged form.
fn plain(value: &DeviceStateValue) -> Value {
    match value {
        DeviceStateValue::Bool(b) => json!(b),
        DeviceStateValue::Int(i) => json!(i),
        DeviceStateValue::Float(f) => json!(f),
        DeviceStateValue::Text(t) => json!(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::device::DeviceId;

    fn power(on: bool) -> DeviceStateChanged {
        DeviceStateChanged {
            device_id: DeviceId::new("matter-1"),
            key: "power".into(),
            value: DeviceStateValue::Bool(on),
            changed_at: chrono::Utc::now(),
        }
    }

    fn seqs(since: Since) -> Option<Vec<u64>> {
        match since {
            Since::Frames(frames) => Some(frames.iter().map(|f| f.seq).collect()),
            Since::Resync => None,
        }
    }

    #[test]
    fn a_frame_carries_its_number_and_a_plain_value() {
        let events = DeviceEvents::starting_at(100);
        assert_eq!(events.push(&power(true)), 100);
        let (since, _) = events.open(Some(99));
        let Since::Frames(frames) = since else {
            panic!("resync")
        };
        assert_eq!(frames[0].data["seq"], 100);
        assert_eq!(frames[0].data["device_id"], "matter-1");
        assert_eq!(frames[0].data["key"], "power");
        assert_eq!(frames[0].data["value"], true);
    }

    #[test]
    fn a_phone_resumes_after_the_last_frame_it_saw() {
        let events = DeviceEvents::starting_at(100);
        for _ in 0..3 {
            events.push(&power(true));
        }
        assert_eq!(seqs(events.open(Some(100)).0), Some(vec![101, 102]));
        assert_eq!(seqs(events.open(Some(102)).0), Some(vec![]));
        assert_eq!(seqs(events.open(None).0), Some(vec![]));
    }

    #[test]
    fn a_number_from_before_a_restart_or_the_ring_is_a_resync() {
        let events = DeviceEvents::starting_at(100);
        assert_eq!(seqs(events.open(Some(42)).0), None, "before this boot");
        assert_eq!(seqs(events.open(Some(5_000)).0), None, "not issued yet");
        for _ in 0..KEEP + 10 {
            events.push(&power(true));
        }
        assert_eq!(
            seqs(events.open(Some(105)).0),
            None,
            "dropped from the ring"
        );
        let oldest = 100 + 10;
        assert_eq!(
            seqs(events.open(Some(oldest - 1)).0).map(|s| s.len()),
            Some(KEEP)
        );
    }

    #[tokio::test]
    async fn a_state_change_on_the_bus_reaches_an_open_stream() {
        let bus: Arc<dyn EventBus> =
            Arc::new(pond_core::shared::services::in_process_event_bus::InProcessEventBus::new());
        let events = Arc::new(DeviceEvents::starting_at(100));
        events.clone().follow(bus.clone());
        let (_, mut live) = events.open(None);
        tokio::task::yield_now().await;
        bus.publish(BusEvent::Device(power(false)));
        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), live.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.data["value"], false);
    }
}
