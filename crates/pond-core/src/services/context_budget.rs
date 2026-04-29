//! Context budget management for constrained LLM inference.
//!
//! On Jetson Orin Nano with a 7B Q4 model, the context window is ~8K tokens.
//! After reserving space for the system prompt and the generated response,
//! roughly 2,500 tokens (~9,952 chars at 4 chars/token) are usable for history.
//!
//! `trim_to_budget()` walks messages newest-first and keeps messages until
//! the character budget is exhausted, then reverses to restore chronological order.
//! This ensures the most recent context is always preserved.

use crate::domain::message::{ChatMessage, Role};
use crate::domain::model_capabilities::ModelCapabilities;

const CHARS_PER_TOKEN: usize = 4;
const MIN_USABLE_HISTORY_CHARS: usize = 256;

/// Maximum assistant tool-output size kept verbatim in history.
pub const TOOL_RESULT_MAX_CHARS: usize = 1_500;

/// Approximate total context window in characters (8K tokens × 4 chars/token).
pub const MAX_CONTEXT_CHARS: usize = 12_000;

/// Characters reserved for the LLM's generated response.
pub const RESERVE_FOR_RESPONSE_CHARS: usize = 2_048;

/// Characters available for conversation history after reserving for response.
pub const USABLE_HISTORY_CHARS: usize = MAX_CONTEXT_CHARS - RESERVE_FOR_RESPONSE_CHARS;

fn truncate_at_byte_budget(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }

    let mut end = max_bytes.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    content[..end].to_string()
}

fn trim_to_char_budget(messages: Vec<ChatMessage>, usable_history_chars: usize) -> Vec<ChatMessage> {
    let mut kept: Vec<ChatMessage> = Vec::new();
    let mut remaining = usable_history_chars;

    for msg in messages.into_iter().rev() {
        let len = msg.content.len();
        if remaining == 0 {
            break;
        }
        if len <= remaining {
            remaining -= len;
            kept.push(msg);
        } else if kept.is_empty() {
            // First (most recent) message exceeds budget — truncate rather than drop.
            let truncated = truncate_at_byte_budget(&msg.content, remaining);
            kept.push(ChatMessage { content: truncated, ..msg });
            break;
        } else {
            // Later messages don't fit — stop here.
            break;
        }
    }

    kept.reverse();
    kept
}

/// Truncate oversized assistant tool outputs before history trimming.
///
/// This targets assistant messages that look like structured tool output
/// payloads (JSON/code blocks) and keeps the first [`TOOL_RESULT_MAX_CHARS`]
/// characters plus a small marker.
pub fn truncate_tool_outputs(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .map(|msg| {
            if msg.role != Role::Assistant || msg.content.len() <= TOOL_RESULT_MAX_CHARS {
                return msg;
            }

            let looks_like_tool_output = msg.content.contains("```json")
                || msg.content.contains("\"tool\"")
                || msg.content.contains("\"result\"")
                || (msg.content.contains('{') && msg.content.contains('}'));

            if !looks_like_tool_output {
                return msg;
            }

            let mut truncated = truncate_at_byte_budget(&msg.content, TOOL_RESULT_MAX_CHARS);
            truncated.push_str("\n\n[tool output truncated]");
            ChatMessage {
                content: truncated,
                ..msg
            }
        })
        .collect()
}

/// Trim a message list to fit within a context-token budget.
///
/// The token limit is converted to an approximate char budget via 4 chars/token,
/// with [`RESERVE_FOR_RESPONSE_CHARS`] held back for model generation.
pub fn trim_to_budget_with_limit(messages: Vec<ChatMessage>, context_limit_tokens: usize) -> Vec<ChatMessage> {
    let usable_history_chars = context_limit_tokens
        .saturating_mul(CHARS_PER_TOKEN)
        .saturating_sub(RESERVE_FOR_RESPONSE_CHARS)
        .max(MIN_USABLE_HISTORY_CHARS);

    trim_to_char_budget(messages, usable_history_chars)
}

/// Trim a message list to fit within [`USABLE_HISTORY_CHARS`].
///
/// Walks messages newest-first, keeping each message until the budget is
/// exhausted.  Returns the surviving messages in chronological (oldest-first)
/// order so they can be passed directly to `LlmProvider::complete()`.
///
/// An individual message that exceeds the entire budget on its own is
/// truncated to `USABLE_HISTORY_CHARS` characters so the caller always
/// receives at least one message.
pub fn trim_to_budget(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    trim_to_char_budget(messages, USABLE_HISTORY_CHARS)
}

/// Trim using the model's actual context window.
///
/// Reserves 20% for the system prompt and generation headroom (minimum 2048 tokens).
/// When `override_tokens` is non-zero, it caps the context window to that value
/// (useful for memory-constrained deployments like Jetson 8GB).
pub fn trim_to_budget_for_model(
    messages: Vec<ChatMessage>,
    capabilities: &ModelCapabilities,
    override_tokens: u32,
) -> Vec<ChatMessage> {
    let token_limit = if override_tokens > 0 {
        override_tokens.min(capabilities.context_window_tokens) as usize
    } else {
        capabilities.context_window_tokens as usize
    };
    // Reserve 20% for system prompt + generation headroom, min 2048 tokens
    let reserved = (token_limit / 5).max(2048);
    let effective = token_limit.saturating_sub(reserved).max(256);
    trim_to_budget_with_limit(messages, effective)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::Role;

    fn msg(content: &str) -> ChatMessage {
        ChatMessage { role: Role::User, content: content.to_string(), images: Vec::new() }
    }

    fn total_chars(msgs: &[ChatMessage]) -> usize {
        msgs.iter().map(|m| m.content.len()).sum()
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(trim_to_budget(vec![]).is_empty());
    }

    #[test]
    fn small_history_passes_through_unchanged() {
        let messages = vec![msg("Hello"), msg("How are you?"), msg("Good thanks")];
        let result = trim_to_budget(messages.clone());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "Hello");
        assert_eq!(result[2].content, "Good thanks");
    }

    #[test]
    fn large_history_is_trimmed_to_budget() {
        // 200 messages × 200 chars each = 40_000 chars >> USABLE_HISTORY_CHARS (9_952)
        let messages: Vec<ChatMessage> =
            (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget(messages);
        assert!(total_chars(&result) <= USABLE_HISTORY_CHARS);
        // Should keep at least 1 message
        assert!(!result.is_empty());
    }

    #[test]
    fn most_recent_messages_are_preserved() {
        // Fill budget with old junk, then add a recent message that fits
        let mut messages: Vec<ChatMessage> =
            (0..60).map(|_| msg(&"a".repeat(200))).collect();
        messages.push(msg("final important message"));

        let result = trim_to_budget(messages);
        // The last message should always be in the result
        assert_eq!(result.last().unwrap().content, "final important message");
    }

    #[test]
    fn single_oversized_message_is_truncated_not_dropped() {
        let big = msg(&"z".repeat(USABLE_HISTORY_CHARS + 1000));
        let result = trim_to_budget(vec![big]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content.len(), USABLE_HISTORY_CHARS);
    }

    #[test]
    fn chronological_order_preserved_after_trim() {
        let messages: Vec<ChatMessage> =
            (0..10u32).map(|i| msg(&format!("message-{}", i))).collect();
        let result = trim_to_budget(messages);
        // Result must be in original order (oldest first)
        for w in result.windows(2) {
            let a: u32 = w[0].content.strip_prefix("message-").unwrap().parse().unwrap();
            let b: u32 = w[1].content.strip_prefix("message-").unwrap().parse().unwrap();
            assert!(a < b, "messages out of order: {} >= {}", a, b);
        }
    }

    #[test]
    fn budget_exactly_full_keeps_all() {
        // Each message is exactly USABLE_HISTORY_CHARS / 4 chars
        let chunk = USABLE_HISTORY_CHARS / 4;
        let messages: Vec<ChatMessage> = (0..4).map(|_| msg(&"m".repeat(chunk))).collect();
        let result = trim_to_budget(messages);
        assert_eq!(result.len(), 4);
        assert_eq!(total_chars(&result), USABLE_HISTORY_CHARS);
    }

    #[test]
    fn trim_to_budget_with_limit_respects_smaller_context() {
        // context_limit_tokens=1024 -> usable chars = 4096-2048=2048
        let messages: Vec<ChatMessage> = (0..20).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_with_limit(messages, 1024);
        assert!(total_chars(&result) <= 2048);
    }

    #[test]
    fn truncate_tool_outputs_truncates_large_assistant_payloads() {
        let messages = vec![
            ChatMessage { role: Role::Assistant, content: format!("{{\"tool\":\"weather\",\"result\":\"{}\"}}", "x".repeat(TOOL_RESULT_MAX_CHARS + 300)), images: Vec::new() },
            msg("normal user message"),
        ];

        let result = truncate_tool_outputs(messages);
        assert_eq!(result.len(), 2);
        assert!(result[0].content.len() <= TOOL_RESULT_MAX_CHARS + 40);
        assert!(result[0].content.contains("[tool output truncated]"));
        assert_eq!(result[1].content, "normal user message");
    }

    #[test]
    fn truncate_tool_outputs_keeps_regular_assistant_text() {
        let plain_assistant = ChatMessage {
            role: Role::Assistant,
            content: "This is a normal answer without tool payload markers.".to_string(),
            images: Vec::new(),
        };
        let result = truncate_tool_outputs(vec![plain_assistant.clone()]);
        assert_eq!(result[0].content, plain_assistant.content);
    }

    #[test]
    fn trim_for_model_uses_reported_context_window() {
        // 128K context → 80% usable = ~102K tokens → ~409K chars
        let caps = ModelCapabilities {
            context_window_tokens: 128_000,
            ..Default::default()
        };
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_for_model(messages.clone(), &caps, 0);
        // With 128K context, all 200 messages (40K chars) should fit easily
        assert_eq!(result.len(), 200);
    }

    #[test]
    fn trim_for_model_with_small_context() {
        // 4K context → 80% = ~3.2K tokens → ~12.8K chars - 2048 reserve
        let caps = ModelCapabilities::default(); // 4096 tokens
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_for_model(messages, &caps, 0);
        // Should trim significantly — 40K chars won't fit in ~6K usable
        assert!(result.len() < 200);
        assert!(!result.is_empty());
    }

    #[test]
    fn trim_for_model_override_caps_context() {
        // Model reports 128K but override limits to 4K
        let caps = ModelCapabilities {
            context_window_tokens: 128_000,
            ..Default::default()
        };
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_for_model(messages, &caps, 4096);
        // Override to 4K should trim just like the small context case
        assert!(result.len() < 200);
    }
}
