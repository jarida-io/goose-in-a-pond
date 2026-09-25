//! Chat pipeline (Wait → Listen → Think → Speak) with test doubles and a wiremock Ollama.

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_ollama::OllamaProvider;
use pond_core::models::domain::message::Role;
use pond_core::models::ports::voice_input::VoiceInput;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
use pond_core::user_data::ports::session_storage::SessionStorage;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Test doubles ──────────────────────────────────────────────────────────────

/// Plays back a script, then `None`, which ends `ChatService::run_loop`.
struct ScriptedVoiceInput {
    script: Mutex<VecDeque<Option<String>>>,
}

impl ScriptedVoiceInput {
    fn new(lines: impl IntoIterator<Item = &'static str>) -> Self {
        let mut deque: VecDeque<Option<String>> =
            lines.into_iter().map(|s| Some(s.to_string())).collect();
        deque.push_back(None);
        Self {
            script: Mutex::new(deque),
        }
    }
}

#[async_trait]
impl VoiceInput for ScriptedVoiceInput {
    async fn listen(&self) -> Result<Option<String>> {
        Ok(self.script.lock().unwrap().pop_front().flatten())
    }
}

/// Collects every string passed to `speak()` so tests can assert on output.
#[derive(Default)]
struct CapturingVoiceOutput {
    spoken: Mutex<Vec<String>>,
}

impl CapturingVoiceOutput {
    fn spoken(&self) -> Vec<String> {
        self.spoken.lock().unwrap().clone()
    }
}

#[async_trait]
impl VoiceOutput for CapturingVoiceOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        self.spoken.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ollama_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": content },
        "done": true
    })
}

// ── Text mode pipeline ────────────────────────────────────────────────────────

/// Uses `run_loop()`, as `chat_once()` never reaches Speak. The spoken text is `MockAgent`'s
/// echo: the `LlmProvider` only titles sessions.
#[tokio::test]
async fn text_mode_listen_think_speak() {
    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "text-mode-test".to_string();
    storage.create_session(session_id.clone()).await.unwrap();
    let output = Arc::new(CapturingVoiceOutput::default());
    let input = Arc::new(ScriptedVoiceInput::new(["What is the capital of France?"]));

    // InstantActivation is the default wake-word detector in ChatService::new()
    let svc = ChatService::new(agent, session_id, storage)
        .with_voice_input(input)
        .with_voice_output(output.clone());

    svc.run_loop().await.unwrap();

    assert!(
        output
            .spoken()
            .iter()
            .any(|s| s.contains("What is the capital of France?")),
        "voice output should have received the agent response; got: {:?}",
        output.spoken()
    );
}

/// Checks `SessionStorage`, not Ollama: chat history lives in the `Agent` port, and the
/// provider only generates titles.
#[tokio::test]
async fn multi_turn_history_preserved_across_chat_once_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_response("A short title.")))
        .mount(&server)
        .await;

    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "multi-turn-test".to_string();
    storage.create_session(session_id.clone()).await.unwrap();
    let provider = Arc::new(OllamaProvider::new(Some(&server.uri()), Some("llama3.2")));

    let svc = ChatService::new(agent, session_id.clone(), storage.clone())
        .with_provider(provider)
        .with_voice_output(Arc::new(CapturingVoiceOutput::default()));

    let first = svc
        .chat_once("What is your name?".to_string())
        .await
        .unwrap();
    let second = svc
        .chat_once("What did you say your name was?".to_string())
        .await
        .unwrap();

    // The Agent port echoes the input.
    assert_eq!(first, "Echo: What is your name?");
    assert_eq!(second, "Echo: What did you say your name was?");

    let messages = storage.get_messages(&session_id).await.unwrap();
    let contents: Vec<&str> = messages
        .iter()
        .map(|m| m.message.content.as_str())
        .collect();
    assert_eq!(
        contents,
        vec![
            "What is your name?",
            "Echo: What is your name?",
            "What did you say your name was?",
            "Echo: What did you say your name was?",
        ],
        "both turns must persist to session storage in order; got: {contents:?}"
    );
}

// ── Message roles ────────────────────────────────────────────────────────────

#[tokio::test]
async fn pipeline_message_roles_are_correct() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_response("42")))
        .mount(&server)
        .await;

    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "roles-test-session".to_string();
    storage.create_session(session_id.clone()).await.unwrap();

    let svc = ChatService::new(agent, session_id.clone(), storage.clone())
        .with_provider(Arc::new(OllamaProvider::new(
            Some(&server.uri()),
            Some("llama3.2"),
        )))
        .with_voice_output(Arc::new(CapturingVoiceOutput::default()));

    svc.chat_once("What is 6 times 7?".to_string())
        .await
        .unwrap();

    let messages = storage.get_messages(&session_id).await.unwrap();
    assert!(messages.len() >= 2, "expected at least 2 messages");
    assert_eq!(
        messages[0].message.role,
        Role::User,
        "first message must be User"
    );
    assert_eq!(
        messages[1].message.role,
        Role::Assistant,
        "second message must be Assistant"
    );
}

// ── Fixture ───────────────────────────────────────────────────────────────────
