//! `OllamaProvider` against a wiremock server; the live smoke test is `#[ignore]`d.

use pond_adapters_ollama::OllamaProvider;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ollama_ok(content: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": content },
        "done": true
    })
}

async fn mount_ok(server: &MockServer, content: &str) {
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_ok(content)))
        .mount(server)
        .await;
}

// ── Request structure ─────────────────────────────────────────────────────────

#[tokio::test]
async fn system_prompt_is_first_message_in_request() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider
        .complete(
            "You are a helpful assistant.",
            vec![ChatMessage::user("Hi")],
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "expected exactly one request");

    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    let first = &body["messages"][0];
    assert_eq!(
        first["role"], "system",
        "first message role must be 'system'"
    );
    assert_eq!(
        first["content"], "You are a helpful assistant.",
        "system message content mismatch"
    );
}

#[tokio::test]
async fn multi_turn_history_sent_in_order() {
    let server = MockServer::start().await;
    mount_ok(&server, "response").await;

    let history = vec![
        ChatMessage::user("first"),
        ChatMessage::assistant("second"),
        ChatMessage::user("third"),
    ];

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider.complete("sys", history).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    // messages[0] is the system prompt; history follows from index 1.
    assert_eq!(body["messages"][1]["content"], "first");
    assert_eq!(body["messages"][2]["content"], "second");
    assert_eq!(body["messages"][3]["content"], "third");
}

// ── Options serialisation ─────────────────────────────────────────────────────

#[tokio::test]
async fn max_tokens_sent_in_options_when_configured() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None).with_max_tokens(128);
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    assert_eq!(
        body["options"]["num_predict"], 128,
        "options.num_predict should be 128"
    );
}

#[tokio::test]
async fn temperature_sent_in_options_when_configured() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None).with_temperature(0.3);
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    let temp = body["options"]["temperature"]
        .as_f64()
        .expect("options.temperature should be a number");
    assert!(
        (temp - 0.3_f64).abs() < 1e-4,
        "expected temperature ≈ 0.3, got {}",
        temp
    );
}

/// Absent `options` lets Ollama use its own defaults.
#[tokio::test]
async fn options_absent_when_not_configured() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    assert!(
        body.get("options").is_none(),
        "options key must be absent when not configured (not null, not empty); got: {}",
        body["options"]
    );
}

// ── Error paths ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn server_error_propagates_as_err() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    let result = provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await;

    assert!(result.is_err(), "expected Err on HTTP 500, got Ok");
}

#[tokio::test]
async fn connection_refused_propagates_as_err() {
    // Port 1 is reserved and will always refuse connections.
    let provider = OllamaProvider::new(Some("http://127.0.0.1:1"), None);
    let result = provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await;

    assert!(
        result.is_err(),
        "expected Err for connection refused, got Ok"
    );
}

// ── Model name ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn model_name_returns_configured_value() {
    let provider = OllamaProvider::new(None, Some("llama3.2"));
    assert_eq!(provider.model_name(), "llama3.2");
}

#[tokio::test]
async fn model_name_in_request_body_matches_configured_model() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), Some("mistral"));
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["model"], "mistral",
        "request body model must match configured model"
    );
}

// ── stream_complete + usage ───────────────────────────────────────────────────

#[tokio::test]
async fn stream_complete_yields_full_text_as_one_chunk() {
    use futures::StreamExt;
    use pond_core::models::ports::provider::StreamToken;

    let server = MockServer::start().await;
    mount_ok(&server, "Hello from Ollama!").await;

    let provider = OllamaProvider::new(Some(&server.uri()), Some("llama3.2"));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("hi")]);

    let mut texts = Vec::new();
    while let Some(Ok(item)) = stream.next().await {
        if let StreamToken::Text(t) = item {
            texts.push(t);
        }
    }

    assert_eq!(texts, vec!["Hello from Ollama!".to_string()]);
}

#[tokio::test]
async fn stream_complete_yields_usage_from_response() {
    use futures::StreamExt;
    use pond_core::models::ports::provider::StreamToken;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "Four." },
            "done": true,
            "prompt_eval_count": 28,
            "eval_count": 5
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(Some(&server.uri()), Some("llama3.2"));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("2+2?")]);

    let mut usage_opt = None;
    while let Some(Ok(item)) = stream.next().await {
        if let StreamToken::Usage(u) = item {
            usage_opt = Some(u);
        }
    }

    let usage = usage_opt.expect("expected a Usage token in stream");
    assert_eq!(usage.prompt_tokens, 28, "prompt_tokens mismatch");
    assert_eq!(usage.completion_tokens, 5, "completion_tokens mismatch");
}

// ── Live smoke test ───────────────────────────────────────────────────────────

/// Run with `cargo test -p pond-adapters-ollama -- --ignored live_ollama`.
#[tokio::test]
#[ignore = "requires local Ollama instance with llama3.2 pulled"]
async fn live_ollama_completion() {
    let provider = OllamaProvider::new(None, Some("llama3.2")); // default: http://localhost:11434
    let reply = provider
        .complete(
            "You are a concise assistant. Reply in one sentence.",
            vec![ChatMessage::user("What is 2 + 2?")],
        )
        .await
        .expect("live Ollama completion failed");

    println!("Ollama replied: {}", reply.content);
    assert!(
        !reply.content.is_empty(),
        "expected non-empty response from Ollama"
    );
}
