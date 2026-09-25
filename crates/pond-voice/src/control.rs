//! Voice-loop control phrases, kept in this leaf crate so the terminal loop, the speculative
//! gate and the desktop shell (a separate workspace) share one list.

/// What a spoken phrase means to the loop itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    /// Stop listening, but stay running — the wake word brings it back.
    Dismissal,
    /// Terminate the voice loop entirely.
    Exit,
    /// Ordinary utterance — hand it to the LLM.
    Normal,
}

/// Matches the whole utterance, never a substring: "stop the kitchen timer" is a request.
pub fn classify(text: &str) -> VoiceCommand {
    let lower = text.trim().to_lowercase();
    let lower = lower.trim_end_matches(|c: char| c == '.' || c == '!');
    match lower {
        "bye" | "goodbye" | "good bye" | "dismissed" | "go to sleep" | "that's all"
        | "thats all" | "never mind" | "nevermind" | "stop" | "stop listening" => {
            VoiceCommand::Dismissal
        }
        "exit" | "quit" => VoiceCommand::Exit,
        _ => VoiceCommand::Normal,
    }
}

/// Any control phrase; the speculative path skips inference on these.
pub fn is_control_phrase(text: &str) -> bool {
    !matches!(classify(text), VoiceCommand::Normal)
}

/// Farewell text and whether it ends the process; `None` for ordinary speech.
pub fn farewell_for(text: &str) -> Option<(&'static str, bool)> {
    match classify(text) {
        VoiceCommand::Dismissal => {
            Some(("Until next time. Just say my name when you need me.", false))
        }
        VoiceCommand::Exit => Some(("Goodbye! I'll be here whenever you need me.", true)),
        VoiceCommand::Normal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissals_are_recognised_whatever_whisper_punctuates_them_with() {
        for phrase in ["bye", "Bye.", "GOODBYE!", "  never mind  ", "go to sleep"] {
            assert_eq!(classify(phrase), VoiceCommand::Dismissal, "{phrase:?}");
        }
    }

    #[test]
    fn exit_ends_the_process_and_dismissal_does_not() {
        assert_eq!(classify("exit"), VoiceCommand::Exit);
        assert_eq!(classify("quit"), VoiceCommand::Exit);
        assert_eq!(farewell_for("exit").unwrap().1, true, "exit terminates");
        assert_eq!(farewell_for("bye").unwrap().1, false, "dismissal sleeps");
    }

    #[test]
    fn a_request_containing_a_control_word_is_not_a_control_phrase() {
        for phrase in [
            "stop the kitchen timer",
            "stop listening to the radio in the lounge",
            "exit the garage light from the hallway group",
            "never mind the weather, what time is it",
        ] {
            assert_eq!(classify(phrase), VoiceCommand::Normal, "{phrase:?}");
            assert!(!is_control_phrase(phrase));
            assert!(farewell_for(phrase).is_none());
        }
    }

    #[test]
    fn ordinary_speech_passes_through() {
        assert_eq!(classify("what is the weather"), VoiceCommand::Normal);
        assert_eq!(classify(""), VoiceCommand::Normal);
    }
}
