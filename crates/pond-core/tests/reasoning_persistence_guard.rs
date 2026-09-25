//! PAI-5 P2 — the join between the counter and the store.
//!
//! Two halves of this were already proven and the seam between them was not.
//! `shared::domain::turn_stats` asserts that a `TurnStats` can *hold* a
//! reasoning count, and `pond-infra`'s `reasoning_tokens_round_trip_beside_the_
//! provider_counts` asserts that a `SessionMessage` carrying one survives
//! SQLite. Both of those build their subject by hand. Neither runs the code
//! that has to put the number there, which is one line in
//! `ChatService::persist_assistant_response`:
//!
//! ```ignore
//! .with_reasoning_tokens(usage.and_then(|u| u.reasoning_tokens))
//! ```
//!
//! Delete that line and every test in the workspace stayed green, because the
//! struct field and the column both still worked perfectly — on rows nobody
//! populated. That is the exact shape of the vacuity failures this programme
//! keeps recording: the gate is tested while its input is not.
//!
//! So this drives the whole path. A fake `Agent` emits an
//! `AgentStreamEvent::Done` shaped exactly like `GooseAdapter`'s (both of its
//! `UsageStats` build arms carry the count), `chat_stream_once` runs for real,
//! and the assertion is on the `SessionMessage` that `ChatService` handed to
//! storage. The fake storage keeps the message VERBATIM on purpose: the claim
//! under test is what `ChatService` constructed, not what an adapter chose to
//! keep.
//!
//! **Scope, deliberately narrow.** This guards the `ChatService` path only —
//! the terminal voice loop and `pond chat`. The desktop `/chat/stream` route
//! persists through `persist_assistant_turn`, which takes a `(prompt,
//! completion)` tuple and cannot express a third number, so rows written by
//! that route keep NULL. That is by design, not an omission, and this file
//! must not be read as covering it.
//!
//! Lives in `tests/` rather than in `chat.rs`'s own `mod tests` so it needs no
//! `test-mocks` feature: `shared::mocks` is `#[cfg(any(test, feature =
//! "test-mocks"))]` and CI runs `cargo test -p pond-core` with no features, so
//! a guard hidden behind that feature would never execute in CI. The fakes are
//! defined locally instead. `MockAgent` could not have been used regardless —
//! it hardcodes `usage: None`, which is precisely the input this is about.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use pond_core::models::domain::message::Role;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::provider::UsageStats;
use pond_core::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::domain::session::{Session, SessionMessage};
use pond_core::user_data::ports::session_storage::{SessionStorage, SessionStorageError};

/// Distinct from each other and the reasoning count, so a wrong-field copy can't pass.
const PROMPT_TOKENS: u32 = 11;
const COMPLETION_TOKENS: u32 = 7;
/// The GIAP-derived reasoning count under test.
const REASONING_TOKENS: u32 = 37;

/// Stores each `SessionMessage` verbatim, so the test asserts on what `ChatService` built.
struct CapturingStorage {
    msgs: Mutex<Vec<SessionMessage>>,
}

impl CapturingStorage {
    fn new() -> Self {
        Self {
            msgs: Mutex::new(Vec::new()),
        }
    }

    fn captured(&self) -> Vec<SessionMessage> {
        self.msgs.lock().expect("storage lock poisoned").clone()
    }
}

#[async_trait]
impl SessionStorage for CapturingStorage {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError> {
        Ok(Session::new(session_id))
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        Ok(Session::new(session_id.to_string()))
    }

    async fn add_message(
        &self,
        _session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        self.msgs
            .lock()
            .expect("storage lock poisoned")
            .push(message.clone());
        Ok(message)
    }

    async fn get_messages(
        &self,
        _session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.captured())
    }

    async fn update_title(
        &self,
        _session_id: &str,
        _title: String,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    async fn delete_session(&self, _session_id: &str) -> Result<(), SessionStorageError> {
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
        Ok(Vec::new())
    }

    async fn get_messages_paginated(
        &self,
        _session_id: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.captured())
    }

    async fn get_recent_messages(
        &self,
        _session_id: &str,
        _limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.captured())
    }
}

/// Its `Done` mirrors `GooseAdapter`'s: text, then `UsageStats` with all three counters.
struct CountingAgent {
    reasoning: Option<u32>,
}

#[async_trait]
impl Agent for CountingAgent {
    async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
        unimplemented!("this guard drives the streaming path only")
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let reasoning = self.reasoning;
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(AgentStreamEvent::Text {
                content: "The pond is calm.".to_string(),
            }),
            Ok(AgentStreamEvent::Done {
                session_id,
                model_role,
                usage: Some(UsageStats {
                    prompt_tokens: PROMPT_TOKENS,
                    completion_tokens: COMPLETION_TOKENS,
                    reasoning_tokens: reasoning,
                }),
                stats: None,
            }),
        ])))
    }
}

/// Runs one real turn and returns what was persisted; the default I/O opens no audio device.
async fn run_one_turn(reasoning: Option<u32>) -> Vec<SessionMessage> {
    let storage = Arc::new(CapturingStorage::new());
    let agent = Arc::new(CountingAgent { reasoning });
    let service = ChatService::new(agent, "guard-session".to_string(), storage.clone());

    service
        .chat_stream_once("How deep is the pond?".to_string())
        .await
        .expect("the turn itself must succeed before its persistence can be judged");

    storage.captured()
}

#[tokio::test]
async fn counted_reasoning_tokens_reach_the_persisted_row() {
    let msgs = run_one_turn(Some(REASONING_TOKENS)).await;

    assert_eq!(
        msgs.len(),
        2,
        "chat_stream_once must persist exactly the user message and the \
         assistant reply; it persisted {} row(s), so the assistant row this \
         guard is about was never written at all",
        msgs.len()
    );

    let assistant = &msgs[1];
    assert_eq!(
        assistant.message.role,
        Role::Assistant,
        "the second persisted row must be the assistant reply; it is {:?}, so \
         the row order this guard indexes into has changed",
        assistant.message.role
    );

    assert_eq!(
        assistant.reasoning_tokens,
        Some(REASONING_TOKENS),
        "the turn counted {REASONING_TOKENS} reasoning tokens and the persisted \
         assistant row carries {:?}; the count reached AgentStreamEvent::Done \
         and was dropped on the way into session_messages — check \
         ChatService::persist_assistant_response still calls \
         .with_reasoning_tokens(usage.and_then(|u| u.reasoning_tokens))",
        assistant.reasoning_tokens
    );

    assert_eq!(
        (assistant.prompt_tokens, assistant.completion_tokens),
        (Some(PROMPT_TOKENS), Some(COMPLETION_TOKENS)),
        "the provider-reported counts must ride alongside the reasoning count \
         unchanged; got {:?}",
        (assistant.prompt_tokens, assistant.completion_tokens)
    );

    // A count stamped on every row of the turn would still pass the assertion above.
    assert_eq!(
        msgs[0].reasoning_tokens, None,
        "the user row must carry no reasoning count; it carries {:?}, which \
         means the count is being stamped on every row of the turn rather than \
         on the assistant reply",
        msgs[0].reasoning_tokens
    );
}

#[tokio::test]
async fn nobody_counted_is_not_counted_zero() {
    let msgs = run_one_turn(None).await;

    let assistant = &msgs[1];

    assert_ne!(
        assistant.reasoning_tokens,
        Some(0),
        "an uncounted turn was persisted as Some(0) — that is a measurement \
         claiming the turn did no thinking, when in fact nothing counted. \
         `None` is not `Some(0)`; anything deriving a budget from this column \
         will read the zero as ground truth"
    );
    assert_eq!(
        assistant.reasoning_tokens, None,
        "an uncounted turn must persist NULL; it persisted {:?}",
        assistant.reasoning_tokens
    );
}
