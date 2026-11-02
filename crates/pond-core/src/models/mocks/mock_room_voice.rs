//! A room voice control a test steers: a status it sets, and events it sends.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::models::domain::room_voice::{
    RoomInterrupts, RoomState, RoomStatus, RoomUnavailable, RoomWake,
};
use crate::models::domain::wake_detector::{WakeDetector, WakeDetectorReason};
use crate::models::ports::room_voice::RoomVoiceControl;
use crate::shared::domain::agent::WorkflowEvent;

pub struct MockRoomVoice {
    status: Mutex<RoomStatus>,
    events: broadcast::Sender<WorkflowEvent>,
    interrupts: AtomicUsize,
}

impl Default for MockRoomVoice {
    fn default() -> Self {
        Self {
            status: Mutex::new(RoomStatus {
                enabled: true,
                available: true,
                unavailable_reason: None,
                state: RoomState::Wait,
                wake: RoomWake {
                    phrase: "hey goose".into(),
                    detector: WakeDetector::Whisper,
                    reason: WakeDetectorReason::NotCalibrated,
                },
                interrupts: RoomInterrupts::WakeOnly,
                last_heard: None,
                session_id: Some("room-session".into()),
                paused_until: None,
            }),
            events: broadcast::channel(64).0,
            interrupts: AtomicUsize::new(0),
        }
    }
}

impl MockRoomVoice {
    pub fn unavailable(reason: RoomUnavailable) -> Self {
        let mock = Self::default();
        {
            let mut s = mock.status.lock().unwrap_or_else(|e| e.into_inner());
            s.available = false;
            s.unavailable_reason = Some(reason);
            s.state = RoomState::Off;
            s.session_id = None;
        }
        mock
    }

    /// Delivered to every subscriber; returns how many there were.
    pub fn send(&self, event: WorkflowEvent) -> usize {
        self.events.send(event).unwrap_or(0)
    }

    pub fn subscribers(&self) -> usize {
        self.events.receiver_count()
    }

    pub fn interrupts(&self) -> usize {
        self.interrupts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RoomVoiceControl for MockRoomVoice {
    fn status(&self) -> RoomStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    async fn set_enabled(&self, enabled: bool) -> RoomStatus {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.enabled = enabled;
        s.state = if enabled && s.available {
            RoomState::Starting
        } else {
            RoomState::Off
        };
        s.clone()
    }

    fn interrupt(&self) {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
    }

    fn subscribe(&self) -> broadcast::Receiver<WorkflowEvent> {
        self.events.subscribe()
    }

    fn pause(&self, span: std::time::Duration) {
        let until = chrono::Utc::now() + chrono::Duration::from_std(span).unwrap_or_default();
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.state = RoomState::Paused;
        s.paused_until = Some(until.to_rfc3339());
    }

    fn resume(&self) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if s.state == RoomState::Paused {
            s.state = RoomState::Wait;
        }
        s.paused_until = None;
    }
}
