//! Why a turn ended without an answer, in words the household can read.
//!
//! A non-streaming turn (a schedule's prompt, `POST /api/v1/chat`) that produces no reply is an
//! error, and that error's text reaches people: a schedule's run history shows it on the
//! dashboard's Schedules history, the Canvas brief card and the phone's Recent runs. The engine's
//! own wording ("Execution error: null result from llama cpp", a list of tool names and a call
//! count) tells them nothing they can act on, so an adapter reports a [`TurnFailure`] instead and
//! keeps that detail in its structured logs.

use std::fmt;

/// Why a non-streaming turn produced no answer.
///
/// An adapter returns this as the turn's error (`anyhow::Error::from(failure)`), so a caller can
/// tell the kind with `error.downcast_ref::<TurnFailure>()` rather than by matching text. It
/// carries no technical detail on purpose: tool names, tool-call counts and the engine's own error
/// belong in the adapter's structured `tracing` on the failure edge, where engineers read them.
///
/// Its `Display` is [`TurnFailure::user_message`], so a caller that stores `error.to_string()`,
/// as the scheduler's run history does, stores the plain sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnFailure {
    /// The model could not load or start: it ran out of GPU or system memory, or the load failed.
    ModelUnavailable,
    /// The model called tools, then ended the turn without writing an answer.
    NoReplyAfterTools,
    /// The model ended the turn without calling a tool or writing an answer.
    NoReply,
    /// The model's engine failed mid-turn for a reason other than the model being unavailable.
    Engine,
}

impl TurnFailure {
    /// The failure of a turn that wrote no answer, after `tool_calls` tool calls.
    pub fn without_reply(tool_calls: usize) -> Self {
        if tool_calls == 0 {
            Self::NoReply
        } else {
            Self::NoReplyAfterTools
        }
    }

    /// One or two plain sentences: what happened and what the person can do about it. Never
    /// names a tool, a library or a count, so it is safe to show anyone in the household.
    pub fn user_message(self) -> &'static str {
        match self {
            Self::ModelUnavailable => {
                "Goose could not start its model, most likely because the Pond ran out of memory. \
                 It will try again next time; if this keeps happening, choose a smaller model on \
                 the Models page."
            }
            Self::NoReplyAfterTools => {
                "Goose looked things up but stopped before writing the answer. Try again, or \
                 reword the request."
            }
            Self::NoReply => {
                "Goose did not write an answer this time. Try again, or reword the request."
            }
            Self::Engine => {
                "Goose's model stopped with an error. Try again; the Pond's logs have the details."
            }
        }
    }
}

impl fmt::Display for TurnFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.user_message())
    }
}

impl std::error::Error for TurnFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TurnFailure; 4] = [
        TurnFailure::ModelUnavailable,
        TurnFailure::NoReplyAfterTools,
        TurnFailure::NoReply,
        TurnFailure::Engine,
    ];

    #[test]
    fn a_turn_without_a_reply_is_told_apart_by_whether_it_called_tools() {
        assert_eq!(TurnFailure::without_reply(0), TurnFailure::NoReply);
        assert_eq!(
            TurnFailure::without_reply(1),
            TurnFailure::NoReplyAfterTools
        );
        assert_eq!(
            TurnFailure::without_reply(13),
            TurnFailure::NoReplyAfterTools
        );
    }

    /// The run history stores `error.to_string()`; that must be the sentence, not a variant name.
    #[test]
    fn the_error_text_is_the_plain_sentence() {
        for failure in ALL {
            assert_eq!(failure.to_string(), failure.user_message());
            let wrapped = anyhow::Error::from(failure);
            assert_eq!(wrapped.downcast_ref::<TurnFailure>(), Some(&failure));
            assert_eq!(wrapped.to_string(), failure.user_message());
        }
    }

    /// The engineer text these replace: no tool names, libraries, counts or error codes.
    #[test]
    fn no_sentence_carries_engineer_text() {
        for failure in ALL {
            let msg = failure.user_message();
            for jargon in [
                "llama",
                "cpp",
                "CUDA",
                "tool call",
                "giap-",
                "__",
                "Execution error",
                "null",
            ] {
                assert!(!msg.contains(jargon), "{failure:?} says {jargon:?}: {msg}");
            }
            assert!(
                !msg.chars().any(|c| c.is_ascii_digit()),
                "{failure:?} has a number: {msg}"
            );
            assert!(msg.ends_with('.'), "{failure:?}: {msg}");
        }
    }
}
