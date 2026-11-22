//! A reply that says it is about to act and then ends the turn without acting.
//!
//! A small local model sometimes writes "I'm searching the web for that, give me a moment" and
//! stops: no tool call follows, so nothing is ever searched and the household waits on a promise.
//! On 2026-11-22 Gemma 4 E4B on the Jetson did exactly that with `search_web` offered. The turn
//! is not empty, so the empty-turn re-engagement never ran.
//!
//! This reads only the model's own reply, after the turn, to decide whether to ask it once more;
//! it never routes or pre-classifies what the household asked. The adapter applies it only to a
//! turn that has run no tool at all, so a reply that says "let me check" between two tool calls,
//! or after one, is never touched.

/// Longer than this is an answer that happens to contain a phrase, not a bare promise.
const MAX_PROMISE_CHARS: usize = 600;

/// First-person phrases of an action that is about to start, as a model writes them in place of
/// calling the tool. Matched on the lower-cased reply with typographic apostrophes made plain.
const PROMISE_PHRASES: &[&str] = &[
    "i'm searching",
    "i am searching",
    "i'm looking",
    "i am looking",
    "i'm checking",
    "i am checking",
    "let me search",
    "let me check",
    "let me look",
    "let me find",
    "i'll search",
    "i will search",
    "i'll check",
    "i will check",
    "i'll look",
    "i will look",
    "i'll find",
    "i will find",
    "searching the web",
    "give me a moment",
    "one moment",
    "just a moment",
];

/// Whether `reply` is a short promise to act that a tool call should have followed.
pub fn promises_unrun_action(reply: &str) -> bool {
    let reply = reply.trim();
    if reply.is_empty() || reply.chars().count() > MAX_PROMISE_CHARS {
        return false;
    }
    let plain = reply.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'");
    PROMISE_PHRASES.iter().any(|phrase| plain.contains(phrase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jetson_reply_that_announced_a_search_and_stopped_is_a_promise() {
        assert!(promises_unrun_action(
            "I'm searching the web for the latest reports on the controversy you mentioned \
             regarding the World Health Organization, the distribution of tetanus vaccines to \
             Kenya, and the fertility-reducing agent. Give me a moment to find the most recent \
             and relevant information."
        ));
        assert!(promises_unrun_action("Let me check the weather for you."));
        assert!(promises_unrun_action("I\u{2019}ll look that up."));
    }

    #[test]
    fn an_answer_is_not_a_promise() {
        assert!(!promises_unrun_action(
            "It is 24 degrees and sunny in Ndenderu."
        ));
        assert!(!promises_unrun_action(""));
        assert!(!promises_unrun_action("Your kitchen light is off."));
    }

    #[test]
    fn a_long_answer_that_mentions_checking_is_not_a_promise() {
        let answer = format!(
            "{} You can check the schedule on the club's site.",
            "Arsenal play Bayern on Tuesday at 22:00. ".repeat(20)
        );
        assert!(answer.chars().count() > MAX_PROMISE_CHARS);
        assert!(!promises_unrun_action(&answer));
    }
}
