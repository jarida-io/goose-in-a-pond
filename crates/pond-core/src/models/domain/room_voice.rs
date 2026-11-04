//! Room voice: serve listening through the pond's own microphone and answering on its speaker.
//! These are the shapes of `GET /api/v1/voice/room`.

use serde::{Deserialize, Serialize};

use super::wake_detector::{WakeDetector, WakeDetectorReason};
use crate::shared::domain::agent::{WorkflowEvent, WorkflowState};

/// Why the room cannot listen even though it is switched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomUnavailable {
    /// No microphone and speaker were shown to work together, or the mic is off in Settings.
    NoMicSpeakerPair,
    /// A `chat --voice` session holds the voice lock.
    VoiceRunningElsewhere,
    /// The speech model, the voice or Silero is not on this pond.
    ModelsMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomState {
    Off,
    Starting,
    Wait,
    Listen,
    Thinking,
    Speak,
    /// Hears no wake word for a while, as while a member teaches it their voice.
    Paused,
}

impl From<WorkflowState> for RoomState {
    fn from(state: WorkflowState) -> Self {
        match state {
            WorkflowState::Wait => Self::Wait,
            WorkflowState::Listen => Self::Listen,
            WorkflowState::Thinking => Self::Thinking,
            WorkflowState::Speak => Self::Speak,
        }
    }
}

/// What may cut a reply short in the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomInterrupts {
    /// An enrolled household voice over the reply, or the wake word.
    EnrolledOrWake,
    /// Nobody is enrolled for Voice Match: the wake word only.
    WakeOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomWake {
    pub phrase: String,
    pub detector: WakeDetector,
    pub reason: WakeDetectorReason,
}

/// The last member Voice Match named on a wake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastHeard {
    pub profile_id: String,
    pub name: String,
    /// RFC 3339.
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomStatus {
    pub enabled: bool,
    /// Whether it could listen now, switched on or not.
    pub available: bool,
    pub unavailable_reason: Option<RoomUnavailable>,
    pub state: RoomState,
    pub wake: RoomWake,
    pub interrupts: RoomInterrupts,
    pub last_heard: Option<LastHeard>,
    pub session_id: Option<String>,
    /// RFC 3339, while `state` is `paused`.
    #[serde(default)]
    pub paused_until: Option<String>,
}

/// The longest pause `POST /voice/room/pause` takes.
pub const MAX_ROOM_PAUSE_SECONDS: u64 = 300;
/// How long the room stays deaf after an enrolment take, for phones that do not pause it.
pub const PAUSE_AFTER_TAKE_SECONDS: u64 = 20;

/// One line of `GET /voice/room/events`: the loop's own events, and the pause the loop never sees.
#[derive(Debug, Clone)]
pub enum RoomEvent {
    Loop(WorkflowEvent),
    /// RFC 3339.
    Paused {
        until: String,
    },
    /// The pause ended, early or on time; `state` is the room's own again.
    Resumed {
        state: RoomState,
    },
}

#[derive(Serialize)]
struct StateLine<'a> {
    event: &'static str,
    state: RoomState,
    #[serde(skip_serializing_if = "Option::is_none")]
    paused_until: Option<&'a str>,
}

impl RoomEvent {
    /// The same NDJSON line `chat --voice --json-events` prints; a pause is a `state` line too.
    pub fn to_ndjson(&self) -> Option<String> {
        let (state, paused_until) = match self {
            Self::Loop(event) => return event.to_ndjson(),
            Self::Paused { until } => (RoomState::Paused, Some(until.as_str())),
            Self::Resumed { state } => (*state, None),
        };
        serde_json::to_string(&StateLine {
            event: "state",
            state,
            paused_until,
        })
        .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_serializes_as_the_phone_reads_it() {
        let status = RoomStatus {
            enabled: true,
            available: false,
            unavailable_reason: Some(RoomUnavailable::VoiceRunningElsewhere),
            state: RoomState::Off,
            wake: RoomWake {
                phrase: "hey goose".into(),
                detector: WakeDetector::Whisper,
                reason: WakeDetectorReason::NotCalibrated,
            },
            interrupts: RoomInterrupts::WakeOnly,
            last_heard: None,
            session_id: None,
            paused_until: None,
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "enabled": true,
                "available": false,
                "unavailable_reason": "voice_running_elsewhere",
                "state": "off",
                "wake": {"phrase": "hey goose", "detector": "whisper", "reason": "not_calibrated"},
                "interrupts": "wake_only",
                "last_heard": null,
                "session_id": null,
                "paused_until": null
            })
        );
        assert_eq!(
            serde_json::to_value(RoomUnavailable::NoMicSpeakerPair).unwrap(),
            "no_mic_speaker_pair"
        );
        assert_eq!(
            serde_json::to_value(RoomUnavailable::ModelsMissing).unwrap(),
            "models_missing"
        );
        assert_eq!(
            serde_json::to_value(RoomInterrupts::EnrolledOrWake).unwrap(),
            "enrolled_or_wake"
        );
        assert_eq!(
            serde_json::to_value(WakeDetector::HeyGoose).unwrap(),
            "hey_goose"
        );
    }

    #[test]
    fn a_pause_and_its_end_are_state_lines_and_the_loops_own_pass_through() {
        assert_eq!(
            RoomEvent::Paused {
                until: "2026-11-03T06:54:45+00:00".into()
            }
            .to_ndjson()
            .unwrap(),
            r#"{"event":"state","state":"paused","paused_until":"2026-11-03T06:54:45+00:00"}"#
        );
        assert_eq!(
            RoomEvent::Resumed {
                state: RoomState::Wait
            }
            .to_ndjson()
            .unwrap(),
            r#"{"event":"state","state":"wait"}"#
        );
        assert_eq!(
            RoomEvent::Loop(WorkflowEvent::StateChanged {
                state: WorkflowState::Speak
            })
            .to_ndjson()
            .unwrap(),
            r#"{"event":"state","state":"speak"}"#
        );
        assert_eq!(
            RoomEvent::Loop(WorkflowEvent::UserInput("hi".into())).to_ndjson(),
            None
        );
    }
}
