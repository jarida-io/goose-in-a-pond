//! A stored tool result must name the call that produced it.
//!
//! Found by forensics, not by a test. Diagnosing a turn that called one tool
//! twenty-five times meant joining tool rows to the assistant message that
//! asked for them, and the join was impossible: across the whole live database
//! there were 508 assistant rows, **none** carrying tool calls, against 488
//! tool rows. Every stored result was an orphan.
//!
//! Neither column was ever written. `ChatMessage::assistant` hardcodes an empty
//! `tool_calls`, and `persist_assistant_turn` passed `String::new()` as the
//! tool-call id -- while the id was right there, inside the JSON blob it wrote
//! into the row's content, and the REST API's history endpoint has always
//! served both fields.
//!
//! What this costs when it is wrong:
//!
//!   * A conversation replayed to a provider that requires a tool result to
//!     follow the call that produced it is malformed. The goose path is
//!     insulated only because goose keeps its own conversation, which is why
//!     nobody noticed.
//!   * The model, on a later turn, reads tool RESULTS with no record that it
//!     was the one who asked -- itself a plausible nudge toward asking again.
//!   * Forensics cannot tie a result to its call, which is the join that had to
//!     be done by hand to find the loop.
//!
//! These assertions are about linkage rather than symbols: the ids written on
//! the tool rows must be exactly the ids named by the assistant row.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use pond_core::models::domain::message::Role;
use pond_core::models::ports::agent::Agent;
use pond_core::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::domain::session::{Session, SessionMessage};
use pond_core::user_data::ports::session_storage::{SessionStorage, SessionStorageError};

#[derive(Default)]
struct RecordingStorage {
    msgs: Mutex<Vec<SessionMessage>>,
}

impl RecordingStorage {
    fn messages(&self) -> Vec<SessionMessage> {
        self.msgs.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl SessionStorage for RecordingStorage {
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
        self.msgs.lock().expect("poisoned").push(message.clone());
        Ok(message)
    }
    async fn get_messages(
        &self,
        _session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.messages())
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
        Ok(self.messages())
    }
    async fn get_recent_messages(
        &self,
        _session_id: &str,
        _limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.messages())
    }
    async fn add_thinking(
        &self,
        _session_id: &str,
        _message_id: &str,
        _blocks: &[String],
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }
    async fn get_thinking_for_session(
        &self,
        _session_id: &str,
    ) -> Result<HashMap<String, Vec<String>>, SessionStorageError> {
        Ok(HashMap::new())
    }
}

struct InertAgent;

#[async_trait]
impl Agent for InertAgent {
    async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
        unimplemented!("this guard drives persistence, not generation")
    }
    async fn chat_stream(
        &self,
        _request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        unimplemented!("this guard drives persistence, not generation")
    }
}

/// The blobs the chat handler builds, in the shape it builds them.
fn tool_result_blob(id: &str, tool: &str, arguments: &str, content: &str) -> String {
    serde_json::json!({
        "tool_call_id": id,
        "tool": tool,
        "content": content,
        "arguments": arguments,
    })
    .to_string()
}

async fn run_turn(tool_results: Vec<String>) -> Arc<RecordingStorage> {
    let storage = Arc::new(RecordingStorage::default());
    let service = ChatService::new(
        Arc::new(InertAgent),
        "linkage-session".to_string(),
        storage.clone(),
    );
    service
        .persist_user_message("what is the weather?")
        .await
        .unwrap();
    service
        .persist_assistant_turn(tool_results, "It is 22C and overcast.", None, None)
        .await
        .unwrap();
    storage
}

#[tokio::test]
async fn a_stored_tool_result_names_the_call_that_produced_it() {
    let storage = run_turn(vec![tool_result_blob(
        "call-1",
        "giap-weather__get_current_weather",
        r#"{"location":"Nairobi"}"#,
        "22C, overcast",
    )])
    .await;

    let tool_rows: Vec<_> = storage
        .messages()
        .into_iter()
        .filter(|m| m.message.role == Role::Tool)
        .collect();
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(
        tool_rows[0].message.tool_call_id.as_deref(),
        Some("call-1"),
        "the tool row carries no tool_call_id, so nothing can tie it to the call that \
         produced it -- the state the whole live database was in"
    );
}

#[tokio::test]
async fn the_assistant_row_records_the_calls_the_turn_made() {
    let storage = run_turn(vec![
        tool_result_blob("call-1", "giap-weather__get_current_weather", "{}", "22C"),
        tool_result_blob("call-2", "music__status", "{}", "Paused"),
    ])
    .await;

    let assistant = storage
        .messages()
        .into_iter()
        .find(|m| m.message.role == Role::Assistant)
        .expect("the turn persists an assistant message");

    let names: Vec<&str> = assistant
        .message
        .tool_calls
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["giap-weather__get_current_weather", "music__status"],
        "the assistant row records no tool calls, so a replayed conversation has tool \
         results arriving from nowhere"
    );
}

/// The point of the pair: every id on a tool row is named by the assistant row,
/// and every id the assistant row names has a result. Either half alone can be
/// satisfied by a constant.
#[tokio::test]
async fn every_result_links_to_a_call_and_every_call_to_a_result() {
    let storage = run_turn(vec![
        tool_result_blob("call-1", "giap-weather__get_current_weather", "{}", "22C"),
        tool_result_blob("call-2", "music__status", "{}", "Paused"),
    ])
    .await;

    let msgs = storage.messages();
    let mut result_ids: Vec<String> = msgs
        .iter()
        .filter(|m| m.message.role == Role::Tool)
        .filter_map(|m| m.message.tool_call_id.clone())
        .collect();
    let mut call_ids: Vec<String> = msgs
        .iter()
        .filter(|m| m.message.role == Role::Assistant)
        .flat_map(|m| m.message.tool_calls.iter().map(|c| c.id.clone()))
        .collect();
    result_ids.sort();
    call_ids.sort();

    assert_eq!(
        result_ids, call_ids,
        "the calls the assistant recorded and the results that were stored do not \
         describe the same set of tool invocations"
    );
    assert!(!call_ids.is_empty(), "the turn recorded no calls at all");
}

/// The arguments are the only part not present in the result event, so they are
/// carried from the call. A record naming a call nobody can reproduce is a
/// weaker record than one that can be replayed.
#[tokio::test]
async fn a_recorded_call_keeps_the_arguments_it_was_made_with() {
    let storage = run_turn(vec![tool_result_blob(
        "call-1",
        "giap-weather__get_current_weather",
        r#"{"location":"Nairobi"}"#,
        "22C",
    )])
    .await;

    let assistant = storage
        .messages()
        .into_iter()
        .find(|m| m.message.role == Role::Assistant)
        .expect("the turn persists an assistant message");
    assert_eq!(
        assistant.message.tool_calls[0].arguments,
        r#"{"location":"Nairobi"}"#
    );
}

/// A blob that does not parse must still persist its content. Losing a result
/// because its envelope was malformed would be a worse failure than the one
/// this file is about.
#[tokio::test]
async fn a_malformed_blob_still_persists_its_content() {
    let storage = run_turn(vec!["not json at all".to_string()]).await;

    let tool_rows: Vec<_> = storage
        .messages()
        .into_iter()
        .filter(|m| m.message.role == Role::Tool)
        .collect();
    assert_eq!(tool_rows.len(), 1, "the result was dropped");
    assert_eq!(tool_rows[0].message.content, "not json at all");
    assert_eq!(
        tool_rows[0].message.tool_call_id.as_deref(),
        Some(""),
        "an unparseable blob has no id to carry, and must not invent one"
    );
}
