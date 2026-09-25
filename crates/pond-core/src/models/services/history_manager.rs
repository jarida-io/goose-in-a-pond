//! Conversation history assembly for context injection.
//!
//! [`HistoryManager`] keeps whole turns newest-first within a character budget; splitting one
//! orphans a tool result. Output is oldest-first for `InferenceProvider::stream_chat`.

use crate::models::domain::message::{ChatMessage, Role};
use crate::user_data::domain::session::SessionMessage;

pub struct HistoryManager {
    history_char_budget: usize,
}

impl HistoryManager {
    pub fn new(history_char_budget: usize) -> Self {
        Self {
            history_char_budget,
        }
    }

    /// Whole turns that fit the budget, in chronological order.
    pub fn build_history(&self, stored: &[SessionMessage]) -> Vec<ChatMessage> {
        if stored.is_empty() {
            return Vec::new();
        }

        let messages: Vec<ChatMessage> = stored.iter().map(|s| s.message.clone()).collect();

        let turns = group_into_turns(&messages);

        let mut kept: Vec<&[ChatMessage]> = Vec::new();
        let mut used_chars = 0usize;

        for turn in turns.iter().rev() {
            let turn_chars: usize = turn.iter().map(|m| m.content.len()).sum();
            if used_chars + turn_chars > self.history_char_budget && !kept.is_empty() {
                break;
            }
            used_chars += turn_chars;
            kept.push(turn);
        }

        kept.reverse();
        kept.into_iter().flat_map(|t| t.iter().cloned()).collect()
    }
}

/// Turns: a user message plus the non-user messages after it. Leading non-user messages drop.
fn group_into_turns(messages: &[ChatMessage]) -> Vec<&[ChatMessage]> {
    let mut turns: Vec<&[ChatMessage]> = Vec::new();
    let mut start = 0;

    for i in 1..=messages.len() {
        let at_end = i == messages.len();
        let next_is_user = !at_end && messages[i].role == Role::User;

        if (next_is_user || at_end) && start < i {
            if messages[start].role == Role::User {
                turns.push(&messages[start..i]);
            }
            start = i;
        }
    }

    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::message::{ChatMessage, Role, ToolCallRecord};
    use crate::user_data::domain::session::SessionMessage;

    fn session_msg(role: Role, content: &str) -> SessionMessage {
        SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test".to_string(),
            message: ChatMessage {
                role,
                content: content.to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            created_at: chrono::Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        }
    }

    #[test]
    fn empty_history_returns_empty() {
        let mgr = HistoryManager::new(10000);
        assert!(mgr.build_history(&[]).is_empty());
    }

    #[test]
    fn single_turn_passes_through() {
        let stored = vec![
            session_msg(Role::User, "hello"),
            session_msg(Role::Assistant, "hi there"),
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[1].content, "hi there");
    }

    #[test]
    fn over_budget_drops_oldest_turn() {
        // Two turns, budget only fits one.
        let stored = vec![
            session_msg(Role::User, &"a".repeat(500)),
            session_msg(Role::Assistant, &"b".repeat(500)),
            session_msg(Role::User, &"c".repeat(500)),
            session_msg(Role::Assistant, &"d".repeat(500)),
        ];
        let result = HistoryManager::new(1001).build_history(&stored);
        // Should keep only the most recent turn (1001 chars > 500+500=1000)
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "c".repeat(500));
    }

    #[test]
    fn tool_calls_kept_with_turn() {
        let stored = vec![
            session_msg(Role::User, "weather?"),
            session_msg(Role::Assistant, ""),
            session_msg(Role::Tool, "sunny"),
            session_msg(Role::Assistant, "It's sunny"),
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn chronological_order_preserved() {
        let stored: Vec<SessionMessage> = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    session_msg(Role::User, &format!("user-{}", i))
                } else {
                    session_msg(Role::Assistant, &format!("asst-{}", i))
                }
            })
            .collect();
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result[0].content, "user-0");
        assert_eq!(result[5].content, "asst-5");
    }

    #[test]
    fn tool_call_metadata_preserved_through_history() {
        let asst_with_call = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test".to_string(),
            message: ChatMessage::assistant_with_tool_calls(
                "calling weather",
                vec![ToolCallRecord {
                    id: "call-1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{}".to_string(),
                }],
            ),
            created_at: chrono::Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        };
        let tool_result = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test".to_string(),
            message: ChatMessage::tool_result("sunny", "call-1"),
            created_at: chrono::Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        };

        let stored = vec![
            session_msg(Role::User, "weather?"),
            asst_with_call,
            tool_result,
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 3);
        assert_eq!(result[1].tool_calls.len(), 1);
        assert_eq!(result[1].tool_calls[0].id, "call-1");
        assert_eq!(result[2].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn leading_orphan_tool_message_is_dropped() {
        let stored = vec![
            session_msg(Role::Tool, "orphan result"),
            session_msg(Role::User, "hi"),
            session_msg(Role::Assistant, "hello"),
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hi");
    }
}
