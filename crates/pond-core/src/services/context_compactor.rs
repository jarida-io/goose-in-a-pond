//! Context compaction via LLM summarisation.
//!
//! When conversation history grows large, `trim_to_budget` simply drops the
//! oldest messages.  `ContextCompactor` does something smarter: once the
//! history approaches `threshold` (default 80%) of the model's context limit
//! it asks the LLM to write a concise summary of older messages, then splices
//! that summary back as a single `User` message while preserving the most
//! recent messages verbatim.
//!
//! If the LLM call fails for any reason, `compact` falls back to
//! `trim_to_budget` so the chat loop is never interrupted.

use crate::domain::message::{ChatMessage, Role};
use crate::ports::provider::LlmProvider;
use crate::services::context_budget::{trim_to_budget, trim_to_budget_for_model, USABLE_HISTORY_CHARS};
use anyhow::Result;

/// 4 characters per token is a common heuristic for English text.
const CHARS_PER_TOKEN: usize = 4;
const KEEP_RECENT_MESSAGES: usize = 6;

pub struct ContextCompactor {
    /// Fraction of the usable history budget that triggers compaction.
    /// Default: 0.80 (trigger when 80% full).
    threshold: f64,
    /// Estimated context limit in tokens for the active model.
    /// Default: derives from USABLE_HISTORY_CHARS.
    context_limit_tokens: usize,
}

impl Default for ContextCompactor {
    fn default() -> Self {
        Self {
            threshold: 0.80,
            context_limit_tokens: USABLE_HISTORY_CHARS / CHARS_PER_TOKEN,
        }
    }
}

impl ContextCompactor {
    pub fn new(threshold: f64, context_limit_tokens: usize) -> Self {
        Self { threshold, context_limit_tokens }
    }

    /// Returns `true` when the total history size has exceeded the threshold.
    pub fn needs_compaction(&self, messages: &[ChatMessage]) -> bool {
        let chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let estimated_tokens = chars / CHARS_PER_TOKEN;
        estimated_tokens as f64 / self.context_limit_tokens as f64 >= self.threshold
    }

    /// Compact history by LLM-summarising older messages.
    ///
    /// Returns messages in chronological (oldest-first) order, ready to pass
    /// directly to `LlmProvider::complete()`.
    ///
    /// Falls back to `trim_to_budget` on any LLM error.
    pub async fn compact(
        &self,
        provider: &dyn LlmProvider,
        messages: Vec<ChatMessage>,
    ) -> Vec<ChatMessage> {
        if messages.is_empty() || !self.needs_compaction(&messages) {
            return messages;
        }

        let keep_count = messages.len().min(KEEP_RECENT_MESSAGES).max(1);

        let split = if messages.len() > keep_count {
            messages.len() - keep_count
        } else {
            return messages;
        };

        let (to_summarise, to_keep) = messages.split_at(split);

        match summarise(provider, to_summarise).await {
            Ok(summary) => {
                let summary_msg = ChatMessage {
                    role: Role::User,
                    content: format!(
                        "[Earlier conversation summary]\n{}\n[End of summary]",
                        summary
                    ),
                    images: Vec::new(),
                };
                let mut result = vec![summary_msg];
                result.extend_from_slice(to_keep);
                result
            }
            Err(e) => {
                tracing::warn!("ContextCompactor LLM call failed ({e}), falling back to trim_to_budget");
                trim_to_budget(messages)
            }
        }
    }
}

async fn summarise(provider: &dyn LlmProvider, messages: &[ChatMessage]) -> Result<String> {
    let conversation_text: String = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            format!("{}: {}", role, m.content)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt_messages = vec![ChatMessage {
        role: Role::User,
        images: Vec::new(),
        content: format!(
            "Summarise the following conversation concisely, preserving all important \
             facts, decisions, and context. Write 3-5 sentences maximum.\n\n{}",
            conversation_text
        ),
    }];

    let system = "You are a helpful assistant. Summarise conversations accurately and concisely.";
    let response = provider.complete(system, prompt_messages).await?;
    Ok(response.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::provider::LlmProvider;
    use crate::services::context_budget::USABLE_HISTORY_CHARS;
    use async_trait::async_trait;

    struct StubProvider {
        summary: String,
        fail: bool,
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        async fn complete(
            &self,
            _system_prompt: &str,
            _messages: Vec<ChatMessage>,
        ) -> anyhow::Result<ChatMessage> {
            if self.fail {
                Err(anyhow::anyhow!("forced failure"))
            } else {
                Ok(ChatMessage {
                    role: Role::Assistant,
                    content: self.summary.clone(),
                    images: Vec::new(),
                })
            }
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }
    }

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage { role, content: content.to_string(), images: Vec::new() }
    }

    fn big_history(count: usize, payload_len: usize) -> Vec<ChatMessage> {
        (0..count)
            .map(|i| {
                let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
                msg(role, &format!("m{}:{}", i, "x".repeat(payload_len)))
            })
            .collect()
    }

    #[test]
    fn needs_compaction_false_for_small_history() {
        let compactor = ContextCompactor::default();
        let messages = vec![msg(Role::User, "hello"), msg(Role::Assistant, "hi there")];
        assert!(!compactor.needs_compaction(&messages));
    }

    #[test]
    fn needs_compaction_true_when_over_threshold() {
        let compactor = ContextCompactor::default();
        // Fill to 85% of usable history
        let size = (USABLE_HISTORY_CHARS as f64 * 0.85) as usize;
        let messages = vec![msg(Role::User, &"x".repeat(size))];
        assert!(compactor.needs_compaction(&messages));
    }

    #[tokio::test]
    async fn compress_noop_when_within_budget() {
        let compactor = ContextCompactor::default();
        let provider = StubProvider {
            summary: "unused".to_string(),
            fail: false,
        };
        let messages = vec![
            msg(Role::User, "hello"),
            msg(Role::Assistant, "hi"),
            msg(Role::User, "quick question"),
        ];

        let result = compactor.compact(&provider, messages.clone()).await;
        assert_eq!(result, messages);
    }

    #[tokio::test]
    async fn compress_summarizes_old_messages() {
        let compactor = ContextCompactor::default();
        let provider = StubProvider {
            summary: "summary of old turns".to_string(),
            fail: false,
        };
        let messages = big_history(100, 200);

        let result = compactor.compact(&provider, messages).await;
        assert_eq!(result.len(), KEEP_RECENT_MESSAGES + 1);
        assert_eq!(result[0].role, Role::User);
        assert!(result[0].content.contains("summary of old turns"));
    }

    #[tokio::test]
    async fn compress_preserves_last_six_verbatim() {
        let compactor = ContextCompactor::default();
        let provider = StubProvider {
            summary: "summary".to_string(),
            fail: false,
        };
        let messages = big_history(100, 220);
        let expected_tail = messages[messages.len() - KEEP_RECENT_MESSAGES..].to_vec();

        let result = compactor.compact(&provider, messages).await;
        let actual_tail = result[result.len() - KEEP_RECENT_MESSAGES..].to_vec();
        assert_eq!(actual_tail, expected_tail);
    }

    #[tokio::test]
    async fn compress_fallback_when_llm_fails() {
        let compactor = ContextCompactor::default();
        let provider = StubProvider {
            summary: "unused".to_string(),
            fail: true,
        };
        let messages = big_history(100, 240);

        let result = compactor.compact(&provider, messages).await;
        let total_chars: usize = result.iter().map(|m| m.content.len()).sum();
        assert!(total_chars <= USABLE_HISTORY_CHARS);
        assert!(!result.is_empty());
        assert!(!result[0].content.contains("[Earlier conversation summary]"));
    }
}
