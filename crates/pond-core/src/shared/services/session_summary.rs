//! Idle rolling-summary refresh, the sole writer of `sessions.rolling_summary`.
//! Run only after `summary_idle_secs` idle (never at startup) and cancelled by the next
//! turn; turns only read the summary, never wait for one.

use std::sync::Arc;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::models::domain::message::{ChatMessage, Role};
use crate::models::ports::provider::LlmProvider;
use crate::user_data::ports::session_storage::SessionStorage;

/// Newest messages left out of the summary: they stay verbatim in the model's history.
const KEEP_RECENT_MESSAGES: usize = 6;

/// Don't bother refreshing for fewer than this many new messages.
const MIN_UNSUMMARIZED_MESSAGES: usize = 4;

/// The summary rides every later turn's context, so the system prompt asks for accuracy first.
/// Its opening must differ from every chat prompt's, or a reused KV prefix is overwritten.
pub const SUMMARY_SYSTEM: &str = "You keep the running summary of one conversation. The \
assistant reads it before every reply as the record of what came before, so every sentence in \
it must be true to the turns.";

/// What the summary must carry. A standing instruction the user reversed is dropped, never kept
/// beside its reversal: the summary of "no web search" then "web search it" once kept the first.
pub const SUMMARY_INSTRUCTIONS: &str = "Rewrite the summary of this conversation so far, folding \
the newer turns into the existing summary.

In plain prose, at most five sentences, say:
- what the conversation is about;
- what was decided or found, with the names and numbers that matter;
- what is still open or was asked and not yet answered;
- any standing instruction the user gave about how to answer (a tool to use or not use, a \
format, a language, a length). When a later turn changes an earlier instruction, keep only the \
later one, and never repeat one the user has since reversed.

Write only what the turns say. No preamble, no headings, no lists.";

#[derive(Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Summary refreshed and persisted; covers through the given message id.
    Refreshed { through_message_id: String },
    /// Not enough unsummarized history to justify a refresh.
    NothingToDo,
    /// A new turn started; the refresh was abandoned before persisting.
    Cancelled,
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::System => "System",
        Role::Tool => "Tool",
    }
}

pub struct SessionSummaryService {
    provider: Arc<dyn LlmProvider>,
    storage: Arc<dyn SessionStorage>,
}

impl SessionSummaryService {
    pub fn new(provider: Arc<dyn LlmProvider>, storage: Arc<dyn SessionStorage>) -> Self {
        Self { provider, storage }
    }

    /// Folds messages past the through-pointer into the summary and advances the pointer.
    /// Skips the newest [`KEEP_RECENT_MESSAGES`]; a cancelled refresh persists nothing.
    pub async fn refresh(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RefreshOutcome> {
        if cancel.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        let (old_summary, through_id) = self
            .storage
            .get_rolling_summary(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("read rolling summary: {e}"))?;

        let messages = self
            .storage
            .get_messages(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("read session messages: {e}"))?;

        // Messages after the through-pointer, excluding the recent tail.
        let start = match &through_id {
            Some(id) => messages
                .iter()
                .position(|m| &m.id == id)
                .map(|i| i + 1)
                .unwrap_or(0),
            None => 0,
        };
        let end = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
        if end <= start || end - start < MIN_UNSUMMARIZED_MESSAGES {
            return Ok(RefreshOutcome::NothingToDo);
        }
        let window = &messages[start..end];
        let new_through_id = window.last().expect("window is non-empty").id.clone();

        let mut transcript = String::new();
        if let Some(old) = &old_summary {
            transcript.push_str("Existing summary of even earlier turns:\n");
            transcript.push_str(old);
            transcript.push_str("\n\nNewer turns to fold in:\n");
        }
        for m in window {
            transcript.push_str(&format!(
                "{}: {}\n",
                role_label(&m.message.role),
                m.message.content
            ));
        }

        let prompt = vec![ChatMessage::user(format!(
            "{SUMMARY_INSTRUCTIONS}\n\n{transcript}"
        ))];
        let system = SUMMARY_SYSTEM;

        // Race cancellation so a new turn reclaims the serial on-device engine at once.
        let response = tokio::select! {
            r = self.provider.complete(system, prompt) => r?,
            _ = cancel.cancelled() => return Ok(RefreshOutcome::Cancelled),
        };

        if cancel.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        let summary = crate::models::services::thought_filter::strip_reasoning(&response.content)
            .trim()
            .to_string();
        if summary.is_empty() {
            return Ok(RefreshOutcome::NothingToDo);
        }
        self.storage
            .set_rolling_summary(session_id, &summary, &new_through_id)
            .await
            .map_err(|e| anyhow::anyhow!("persist rolling summary: {e}"))?;

        Ok(RefreshOutcome::Refreshed {
            through_message_id: new_through_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::session::SessionMessage;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct StubProvider {
        reply: String,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        async fn complete(
            &self,
            _system_prompt: &str,
            messages: Vec<ChatMessage>,
        ) -> Result<ChatMessage> {
            self.calls.lock().unwrap().push(
                messages
                    .first()
                    .map(|m| m.content.clone())
                    .unwrap_or_default(),
            );
            Ok(ChatMessage::assistant(self.reply.clone()))
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }
    }

    async fn seed(storage: &InMemorySessionStorage, session: &str, n: usize) {
        storage.create_session(session.to_string()).await.unwrap();
        for i in 0..n {
            let role_user = i % 2 == 0;
            let msg = if role_user {
                ChatMessage::user(format!("question {i}"))
            } else {
                ChatMessage::assistant(format!("answer {i}"))
            };
            storage
                .add_message(
                    session.to_string(),
                    SessionMessage::new(format!("m{i}"), session.to_string(), msg),
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn refresh_summarizes_and_advances_pointer() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 12).await;
        let provider = Arc::new(StubProvider {
            reply: "They discussed twelve things.".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc.refresh("s", &CancellationToken::new()).await.unwrap();
        // 12 messages - 6 recent = window of 6 → through m5.
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                through_message_id: "m5".to_string()
            }
        );
        let (summary, through) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(summary.as_deref(), Some("They discussed twelve things."));
        assert_eq!(through.as_deref(), Some("m5"));
    }

    #[tokio::test]
    async fn refresh_merges_existing_summary_into_prompt() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 16).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks were discussed.", "m3")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "Ducks and geese.".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc.refresh("s", &CancellationToken::new()).await.unwrap();
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                through_message_id: "m9".to_string()
            }
        );
        let sent = provider.calls.lock().unwrap();
        assert!(sent[0].contains("Earlier: ducks were discussed."));
        assert!(sent[0].contains("question 4"));
    }

    #[test]
    fn the_prompt_asks_for_the_topic_the_findings_what_is_open_and_the_latest_instruction() {
        assert!(SUMMARY_INSTRUCTIONS.contains("what the conversation is about"));
        assert!(SUMMARY_INSTRUCTIONS.contains("what was decided or found"));
        assert!(SUMMARY_INSTRUCTIONS.contains("what is still open"));
        assert!(SUMMARY_INSTRUCTIONS.contains("keep only the"));
        assert!(SUMMARY_INSTRUCTIONS.contains("at most five sentences"));
    }

    #[tokio::test]
    async fn a_reasoning_block_never_reaches_the_stored_summary() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 12).await;
        let provider = Arc::new(StubProvider {
            reply: "<think>what did they say</think>They compared two cars.".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider, storage.clone());
        svc.refresh("s", &CancellationToken::new()).await.unwrap();
        let (summary, _) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(summary.as_deref(), Some("They compared two cars."));
    }

    #[tokio::test]
    async fn too_little_new_history_is_a_noop() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 8).await; // 8 - 6 recent = 2 < MIN 4
        let provider = Arc::new(StubProvider {
            reply: "unused".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());
        let outcome = svc.refresh("s", &CancellationToken::new()).await.unwrap();
        assert_eq!(outcome, RefreshOutcome::NothingToDo);
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_refresh_persists_nothing() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 12).await;
        let provider = Arc::new(StubProvider {
            reply: "should never land".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider, storage.clone());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = svc.refresh("s", &cancel).await.unwrap();
        assert_eq!(outcome, RefreshOutcome::Cancelled);
        let (summary, _) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(summary, None);
    }
}
