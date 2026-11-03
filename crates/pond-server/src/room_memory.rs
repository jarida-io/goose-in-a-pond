//! `room voice memory`: what the room holds on this board, part by part, logged so its cost is
//! read from the device rather than guessed. A part's growth is the whole process's across one
//! of its calls, so another thread allocating meanwhile counts too: an upper bound.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_ear::{process_memory, EarPart, ProcessMemory};
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_voice::text::SpeechUnit;

/// A call that grows the process by this much is logged with its part; smaller growth is noise.
const GROWTH_LOGGED: u64 = 8 * 1024 * 1024;

fn mb(bytes: u64) -> f64 {
    (bytes as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0
}

pub fn now() -> Option<ProcessMemory> {
    process_memory()
}

/// Runs `f`, logging `part` if the process grew by [`GROWTH_LOGGED`] or more meanwhile.
pub fn measured<T>(part: &'static str, f: impl FnOnce() -> T) -> T {
    let before = now();
    let out = f();
    note_growth(part, before);
    out
}

fn note_growth(part: &'static str, before: Option<ProcessMemory>) {
    let (Some(before), Some(after)) = (before, now()) else {
        return;
    };
    let grew = after.footprint.saturating_sub(before.footprint);
    if grew >= GROWTH_LOGGED {
        tracing::info!(
            kind = "room_voice_memory",
            part,
            grew_mb = mb(grew),
            footprint_mb = mb(after.footprint),
            resident_mb = mb(after.resident),
            "room voice memory: {part} grew the process"
        );
    }
}

/// What the ear's parts are; `ear_parts` is empty on the Whisper front end.
pub fn describe_parts(ear_parts: &[EarPart]) -> String {
    ear_parts
        .iter()
        .map(|p| format!("{} {:.1} MB", p.role, p.bytes as f64 / (1024.0 * 1024.0)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The members' wake templates and references as installed: what they hold, and what building
/// them grew the process by.
pub fn log_personal(before: Option<ProcessMemory>, wake: &pond_adapters_ear::PersonalWake) {
    let after = now().unwrap_or_default();
    let grew = before.map_or(0, |b| after.footprint.saturating_sub(b.footprint));
    tracing::info!(
        kind = "room_voice_memory",
        part = "personal_wake",
        held_mb = mb(wake.bytes() as u64),
        grew_mb = mb(grew),
        members = wake.members.len(),
        templates = wake
            .members
            .iter()
            .map(|m| m.templates.len())
            .sum::<usize>(),
        references = wake.references.len(),
        footprint_mb = mb(after.footprint),
        "room voice memory: members' wake templates"
    );
}

/// Once the room is listening: the process before and after its front end, and what that holds.
pub fn log_start(
    before: Option<ProcessMemory>,
    ear_parts: &[EarPart],
    whisper: Option<(Vec<&'static str>, usize)>,
) {
    let after = now().unwrap_or_default();
    let before = before.unwrap_or_default();
    let ear: usize = ear_parts.iter().map(|p| p.bytes).sum();
    let (states, created) = whisper.unwrap_or_default();
    tracing::info!(
        kind = "room_voice_memory",
        part = "start",
        footprint_mb = mb(after.footprint),
        resident_mb = mb(after.resident),
        grew_mb = mb(after.footprint.saturating_sub(before.footprint)),
        ear_mb = mb(ear as u64),
        ear_parts = %describe_parts(ear_parts),
        whisper_states = ?states,
        whisper_states_created = created,
        "room voice memory: listening"
    );
}

/// After each room turn: the process now, and how far it has grown since the room started.
pub fn log_turn(started: Option<ProcessMemory>, whisper: Option<(Vec<&'static str>, usize)>) {
    let Some(after) = now() else {
        return;
    };
    let (states, created) = whisper.unwrap_or_default();
    tracing::info!(
        kind = "room_voice_memory",
        part = "turn",
        footprint_mb = mb(after.footprint),
        resident_mb = mb(after.resident),
        since_start_mb = started.map(|s| mb(after.footprint.saturating_sub(s.footprint))),
        whisper_states = ?states,
        whisper_states_created = created,
        "room voice memory: after a turn"
    );
}

/// The room's voice, with each synthesis measured as the `kokoro` part.
pub struct MeasuredVoice(pub Arc<dyn VoiceOutput>);

#[async_trait]
impl VoiceOutput for MeasuredVoice {
    async fn speak(&self, text: &str) -> Result<()> {
        let before = now();
        let spoken = self.0.speak(text).await;
        note_growth("kokoro", before);
        spoken
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        let before = now();
        let wav = self.0.synthesize(text).await;
        note_growth("kokoro", before);
        wav
    }

    async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
        let before = now();
        let wav = self.0.synthesize_unit(unit).await;
        note_growth("kokoro", before);
        wav
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        self.0.play_audio(audio).await
    }

    fn begin_utterance(&self) {
        self.0.begin_utterance();
    }

    fn end_utterance(&self) {
        self.0.end_utterance();
    }

    fn stop_speaking(&self) {
        self.0.stop_speaking();
    }

    fn is_playing(&self) -> Option<bool> {
        self.0.is_playing()
    }

    fn start_thinking_tone(&self) {
        self.0.start_thinking_tone();
    }

    fn stop_thinking_tone(&self) {
        self.0.stop_thinking_tone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parts_are_named_with_their_size() {
        let parts = [
            EarPart {
                role: "front end",
                name: "32 s ring + fbank stream".into(),
                bytes: 3 * 1024 * 1024,
            },
            EarPart {
                role: "voice activity",
                name: "Silero v5".into(),
                bytes: 1024 * 1024 + 300 * 1024,
            },
        ];
        assert_eq!(
            describe_parts(&parts),
            "front end 3.0 MB, voice activity 1.3 MB"
        );
    }

    #[test]
    fn this_process_can_read_what_it_holds() {
        let before = now().expect("process memory is readable on macOS and Linux");
        let held = measured("test", || vec![1u8; 32 * 1024 * 1024]);
        let after = now().unwrap();
        assert!(held.iter().all(|b| *b == 1));
        assert!(
            after.footprint >= before.footprint,
            "{before:?} then {after:?}"
        );
    }
}
