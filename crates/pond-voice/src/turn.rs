//! The voice turn as an explicit state machine.
//!
//! The pure decision half of `ChatService::run_loop`. The loop keeps the I/O and the
//! exactly-once persistence invariant, which is deliberately not modelled here.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    /// Idle, waiting for the wake word; the initial state.
    Wait,
    /// Microphone open, capturing an utterance.
    Listen,
    /// Model is working; pipelined TTS may already be speaking, so barge-in applies here too.
    Think,
    Speak,
    /// Terminal. The loop exits.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// Wake word fired, possibly with speech the detector kept recording past the trigger.
    Activated { has_captured_audio: bool },
    /// A talk control or the global hotkey started a turn, skipping the wake word.
    PushToTalk,
    /// An utterance transcribed to something usable.
    Transcript(String),
    /// Capture produced nothing: silence, or only whisper artifacts.
    Empty,
    /// A dismissal phrase ("goodbye", "never mind"): ends the exchange, not the session.
    Dismissed,
    /// A hard exit phrase ("quit"), or stdin EOF.
    Exit,
    /// The reply finished normally.
    Replied,
    /// The user spoke over the assistant.
    BargedIn,
    /// Something failed. Non-fatal: the session continues.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAction {
    ArmWakeWord,
    StartCapture,
    /// Transcribe the audio captured with the wake word instead of re-recording.
    UsePrimedAudio,
    Infer(String),
    StopSpeaking,
    /// Clear the turn's interrupt state before any speech.
    BeginUtterance,
    /// Surface a message to the user; a failure must never be silent.
    Report(String),
    /// Persist the turn; exactly once per completed turn, never after an interruption.
    Finalize,
    /// Release the microphone and any audio device.
    ReleaseAudio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub state: VoiceState,
    pub actions: Vec<VoiceAction>,
}

impl Transition {
    fn to(state: VoiceState, actions: Vec<VoiceAction>) -> Self {
        Self { state, actions }
    }
}

/// Advance the machine. Total: an unexpected pair holds position and emits nothing.
pub fn next(state: VoiceState, event: VoiceEvent) -> Transition {
    use VoiceAction as A;
    use VoiceEvent as E;
    use VoiceState as S;

    match (state, event) {
        // Exit wins from anywhere, and always releases the device.
        (_, E::Exit) => Transition::to(S::Closed, vec![A::StopSpeaking, A::ReleaseAudio]),

        // ── Wait ──────────────────────────────────────────────────────────
        (S::Wait, E::Activated { has_captured_audio }) => Transition::to(
            S::Listen,
            if has_captured_audio {
                vec![A::UsePrimedAudio]
            } else {
                vec![A::StartCapture]
            },
        ),
        (S::Wait, E::PushToTalk) => Transition::to(S::Listen, vec![A::StartCapture]),

        // ── Listen ────────────────────────────────────────────────────────
        (S::Listen, E::Transcript(text)) => {
            Transition::to(S::Think, vec![A::BeginUtterance, A::Infer(text)])
        }
        // Nothing said: back to the wake word rather than inferring on silence.
        (S::Listen, E::Empty) => Transition::to(S::Wait, vec![A::ArmWakeWord]),
        (S::Listen, E::Dismissed) => Transition::to(S::Wait, vec![A::ArmWakeWord]),

        // ── Think / Speak ─────────────────────────────────────────────────
        // TTS is pipelined, so both states accept barge-in.
        (S::Think, E::Replied) | (S::Speak, E::Replied) => {
            // Turn-taking: listen again without requiring the wake word.
            Transition::to(S::Listen, vec![A::Finalize, A::StartCapture])
        }
        (S::Think, E::BargedIn) | (S::Speak, E::BargedIn) => {
            // No Finalize: an interrupted turn persists nothing.
            Transition::to(S::Listen, vec![A::StopSpeaking, A::StartCapture])
        }
        (S::Think, E::Dismissed) | (S::Speak, E::Dismissed) => {
            Transition::to(S::Wait, vec![A::StopSpeaking, A::ArmWakeWord])
        }

        // ── Failure ───────────────────────────────────────────────────────
        // Never silent, never fatal: the user can simply try again.
        (_, E::Failed(why)) => Transition::to(
            S::Wait,
            vec![A::StopSpeaking, A::Report(why), A::ArmWakeWord],
        ),

        // Push-to-talk mid-reply means "stop and listen": a barge-in.
        (S::Think, E::PushToTalk) | (S::Speak, E::PushToTalk) => {
            Transition::to(S::Listen, vec![A::StopSpeaking, A::StartCapture])
        }

        (S::Closed, _) => Transition::to(S::Closed, vec![]),

        // Anything else is out of order — hold position rather than guessing.
        (s, _) => Transition::to(s, vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::VoiceAction::*;
    use super::VoiceEvent as E;
    use super::VoiceState as S;
    use super::*;

    fn step(s: VoiceState, e: VoiceEvent) -> Transition {
        next(s, e)
    }

    // ── the happy path ───────────────────────────────────────────────────

    #[test]
    fn a_full_turn_walks_wait_listen_think_and_back_to_listen() {
        let t = step(
            S::Wait,
            E::Activated {
                has_captured_audio: false,
            },
        );
        assert_eq!(t.state, S::Listen);
        assert_eq!(t.actions, vec![StartCapture]);

        let t = step(t.state, E::Transcript("what is the weather".into()));
        assert_eq!(t.state, S::Think);
        assert_eq!(
            t.actions,
            vec![BeginUtterance, Infer("what is the weather".into())],
            "interrupt state must be cleared before any speech"
        );

        let t = step(t.state, E::Replied);
        assert_eq!(t.state, S::Listen, "conversational turn-taking");
        assert!(t.actions.contains(&Finalize));
    }

    #[test]
    fn captured_wake_audio_is_reused_rather_than_recording_again() {
        let t = step(
            S::Wait,
            E::Activated {
                has_captured_audio: true,
            },
        );
        assert_eq!(t.actions, vec![UsePrimedAudio]);
        assert!(!t.actions.contains(&StartCapture));
    }

    // ── push-to-talk ─────────────────────────────────────────────────────

    #[test]
    fn push_to_talk_starts_a_turn_without_the_wake_word() {
        let t = step(S::Wait, E::PushToTalk);
        assert_eq!(t.state, S::Listen);
        assert_eq!(t.actions, vec![StartCapture]);
    }

    #[test]
    fn push_to_talk_during_a_reply_interrupts_it() {
        for from in [S::Think, S::Speak] {
            let t = step(from, E::PushToTalk);
            assert_eq!(t.state, S::Listen, "from {from:?}");
            assert!(t.actions.contains(&StopSpeaking), "from {from:?}");
            assert!(
                !t.actions.contains(&Finalize),
                "must not persist, from {from:?}"
            );
        }
    }

    // ── barge-in ─────────────────────────────────────────────────────────

    #[test]
    fn a_barge_in_never_finalizes_the_turn() {
        for from in [S::Think, S::Speak] {
            let t = step(from, E::BargedIn);
            assert_eq!(t.state, S::Listen, "from {from:?}");
            assert!(t.actions.contains(&StopSpeaking));
            assert!(
                !t.actions.contains(&Finalize),
                "an interrupted turn must persist nothing (from {from:?})"
            );
        }
    }

    #[test]
    fn barge_in_is_accepted_while_still_thinking() {
        let t = step(S::Think, E::BargedIn);
        assert_eq!(t.state, S::Listen);
        assert!(t.actions.contains(&StopSpeaking));
    }

    // ── nothing said ─────────────────────────────────────────────────────

    #[test]
    fn an_empty_capture_returns_to_the_wake_word_without_inferring() {
        let t = step(S::Listen, E::Empty);
        assert_eq!(t.state, S::Wait);
        assert_eq!(t.actions, vec![ArmWakeWord]);
        assert!(
            !t.actions.iter().any(|a| matches!(a, Infer(_))),
            "must never infer on silence"
        );
    }

    // ── dismissal and exit ───────────────────────────────────────────────

    #[test]
    fn dismissal_ends_the_exchange_but_not_the_session() {
        for from in [S::Listen, S::Think, S::Speak] {
            let t = step(from, E::Dismissed);
            assert_eq!(t.state, S::Wait, "from {from:?}");
            assert!(t.actions.contains(&ArmWakeWord));
            assert_ne!(t.state, S::Closed, "dismissal is not exit");
        }
    }

    #[test]
    fn exit_closes_from_any_state_and_releases_the_device() {
        for from in [S::Wait, S::Listen, S::Think, S::Speak] {
            let t = step(from, E::Exit);
            assert_eq!(t.state, S::Closed, "from {from:?}");
            assert!(
                t.actions.contains(&ReleaseAudio),
                "must not leave the mic open (from {from:?})"
            );
        }
    }

    #[test]
    fn closed_is_terminal() {
        for e in [
            E::Activated {
                has_captured_audio: true,
            },
            E::PushToTalk,
            E::Replied,
            E::BargedIn,
        ] {
            let t = step(S::Closed, e.clone());
            assert_eq!(t.state, S::Closed, "{e:?} must not revive a closed session");
            assert!(t.actions.is_empty());
        }
    }

    // ── failure is always visible ────────────────────────────────────────

    #[test]
    fn a_failure_is_always_reported_and_never_fatal() {
        for from in [S::Wait, S::Listen, S::Think, S::Speak] {
            let t = step(from, E::Failed("piper voice not installed".into()));
            assert_eq!(t.state, S::Wait, "from {from:?}");
            assert!(
                t.actions
                    .iter()
                    .any(|a| matches!(a, Report(m) if m.contains("piper"))),
                "the reason must reach the user (from {from:?})"
            );
            assert!(
                t.actions.contains(&ArmWakeWord),
                "recoverable, from {from:?}"
            );
            assert_ne!(t.state, S::Closed, "non-fatal, from {from:?}");
        }
    }

    // ── totality ─────────────────────────────────────────────────────────

    #[test]
    fn every_state_event_pair_is_defined() {
        let states = [S::Wait, S::Listen, S::Think, S::Speak, S::Closed];
        let events = [
            E::Activated {
                has_captured_audio: false,
            },
            E::Activated {
                has_captured_audio: true,
            },
            E::PushToTalk,
            E::Transcript("x".into()),
            E::Empty,
            E::Dismissed,
            E::Exit,
            E::Replied,
            E::BargedIn,
            E::Failed("e".into()),
        ];
        for s in states {
            for e in &events {
                let t = next(s, e.clone());
                if t.actions.contains(&Finalize) {
                    assert!(
                        matches!(e, E::Replied),
                        "Finalize emitted for {e:?} from {s:?} — would persist an \
                         unfinished or interrupted turn"
                    );
                }
            }
        }
    }

    #[test]
    fn an_out_of_order_event_is_ignored_rather_than_resetting() {
        let t = step(S::Wait, E::Replied);
        assert_eq!(t.state, S::Wait);
        assert!(t.actions.is_empty());

        let t = step(
            S::Listen,
            E::Activated {
                has_captured_audio: false,
            },
        );
        assert_eq!(t.state, S::Listen, "already listening");
        assert!(t.actions.is_empty());
    }

    #[test]
    fn finalize_comes_only_from_a_completed_reply() {
        let mut finalizing = Vec::new();
        for s in [S::Wait, S::Listen, S::Think, S::Speak, S::Closed] {
            for e in [
                E::Replied,
                E::BargedIn,
                E::Dismissed,
                E::Exit,
                E::Empty,
                E::PushToTalk,
                E::Failed("x".into()),
            ] {
                if next(s, e.clone()).actions.contains(&Finalize) {
                    finalizing.push((s, e));
                }
            }
        }
        assert_eq!(
            finalizing.len(),
            2,
            "expected Think+Replied and Speak+Replied only, got {finalizing:?}"
        );
    }
}
