//! Request classifier — pure function that maps a user message to a `ModelRole`.
//!
//! No I/O, no async. Fully deterministic and testable.
//!
//! ## Priority order
//! 1. "think about" voice/text prefix → `Think` (highest)
//! 2. Any Think keyword match → `Think`
//! 3. Any Task keyword match → `Task`
//! 4. Default → `Chat`

use crate::domain::model_role::ModelRole;

/// Common wake-word prefixes to strip before keyword matching.
const WAKE_PREFIXES: &[&str] = &[
    "goose, ", "goose ", "hey goose, ", "hey goose ", "ok computer, ", "ok computer ",
];

/// Keywords that indicate a request needs deeper reasoning.
const THINK_KEYWORDS: &[&str] = &[
    "think about",
    "explain why",
    "explain how",
    "analyze",
    "analyse",
    "reason through",
    "compare",
    "evaluate",
    "debate",
    "pros and cons",
    "step by step",
    "how does",
    "why does",
    "why is",
    "what causes",
    "break down",
    "walk me through",
    "deep dive",
];

/// Keywords that indicate an agentic / tool-use request.
const TASK_KEYWORDS: &[&str] = &[
    "schedule",
    "remind me",
    "set alarm",
    "set a timer",
    "add to calendar",
    "turn on",
    "turn off",
    "switch on",
    "switch off",
    "search for",
    "look up",
    "run ",
    "execute",
    "create a ",
    "list devices",
    "list schedules",
    "add device",
];

/// Classify a user message into the most appropriate `ModelRole`.
///
/// The input is the raw message as typed or transcribed.  Leading whitespace
/// and wake-word prefixes are stripped before matching.
pub fn classify_request(message: &str) -> ModelRole {
    let lower = message.trim().to_lowercase();

    // Strip wake-word prefix
    let stripped = WAKE_PREFIXES
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower);

    // "think about …" prefix override — highest priority
    if stripped.starts_with("think about ") || stripped.starts_with("think about,") {
        return ModelRole::Think;
    }

    // Think keyword scan
    for kw in THINK_KEYWORDS {
        if stripped.contains(kw) {
            return ModelRole::Think;
        }
    }

    // Task keyword scan
    for kw in TASK_KEYWORDS {
        if stripped.contains(kw) {
            return ModelRole::Task;
        }
    }

    ModelRole::Chat
}

/// Check if a message is a factual/knowledge question that the Tool Agent
/// should handle (Wikipedia lookup, weather, etc.) before the main LLM runs.
///
/// Deliberately simple — keyword matching is instant and avoids model inference.
pub fn needs_tool_call(message: &str) -> bool {
    let mut stripped = message.to_lowercase();
    for prefix in WAKE_PREFIXES {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            stripped = rest.to_string();
        }
    }
    let stripped = stripped.trim();

    const KNOWLEDGE_PREFIXES: &[&str] = &[
        "who is", "who was", "who are",
        "what is", "what are", "what was", "what were",
        "where is", "where are", "where was",
        "when was", "when did", "when is",
        "tell me about", "explain ", "describe ",
        "how does", "how do", "how did",
        "look up", "search for", "search ", "define ",
        "can you tell me about",
    ];

    KNOWLEDGE_PREFIXES.iter().any(|p| stripped.starts_with(p))
        || stripped.contains("wikipedia")
        || stripped.contains("weather")
        || stripped.contains("temperature")
        || stripped.contains("forecast")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_greeting_is_chat() {
        assert_eq!(classify_request("What time is it?"), ModelRole::Chat);
        assert_eq!(classify_request("Hello"), ModelRole::Chat);
        assert_eq!(classify_request("Tell me a joke"), ModelRole::Chat);
    }

    #[test]
    fn think_keywords_route_to_think() {
        assert_eq!(classify_request("explain why the sky is blue"), ModelRole::Think);
        assert_eq!(classify_request("analyze this situation"), ModelRole::Think);
        assert_eq!(classify_request("compare these two options"), ModelRole::Think);
        assert_eq!(classify_request("what causes inflation"), ModelRole::Think);
        assert_eq!(classify_request("pros and cons of remote work"), ModelRole::Think);
        assert_eq!(classify_request("step by step how to bake bread"), ModelRole::Think);
    }

    #[test]
    fn think_prefix_overrides_all() {
        // Even if task keywords are present, "think about" prefix wins
        assert_eq!(classify_request("think about scheduling my week"), ModelRole::Think);
        assert_eq!(classify_request("Goose, think about why I'm busy"), ModelRole::Think);
    }

    #[test]
    fn task_keywords_route_to_task() {
        assert_eq!(classify_request("remind me to take my meds at 9am"), ModelRole::Task);
        assert_eq!(classify_request("schedule a meeting tomorrow"), ModelRole::Task);
        assert_eq!(classify_request("turn on the living room lights"), ModelRole::Task);
        assert_eq!(classify_request("set alarm for 7am"), ModelRole::Task);
        assert_eq!(classify_request("search for the latest news"), ModelRole::Task);
        assert_eq!(classify_request("list devices"), ModelRole::Task);
    }

    #[test]
    fn wake_word_prefix_is_stripped() {
        assert_eq!(classify_request("Goose, explain why we sleep"), ModelRole::Think);
        assert_eq!(classify_request("hey goose, remind me at noon"), ModelRole::Task);
        assert_eq!(classify_request("Hey Goose, what is the weather"), ModelRole::Chat);
    }

    #[test]
    fn think_beats_task_in_body() {
        // If a message has both think and task keywords, Think wins
        assert_eq!(
            classify_request("analyze and then schedule the best approach"),
            ModelRole::Think
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(classify_request("EXPLAIN WHY dogs bark"), ModelRole::Think);
        assert_eq!(classify_request("SCHEDULE a meeting"), ModelRole::Task);
    }
}
