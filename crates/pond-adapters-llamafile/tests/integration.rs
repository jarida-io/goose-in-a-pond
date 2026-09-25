//! LlamafileProvider against a wiremock llamafile server; no real binary needed.

use futures::StreamExt;
use pond_adapters_llamafile::{LlamafileProvider, DEFAULT_MODEL};
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::LlmProvider;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Response helpers ──────────────────────────────────────────────────────────

fn success_body(content: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{"message": {"content": content}}]
    })
}

/// Build a well-formed OpenAI SSE body with the given token strings.
fn sse_body(tokens: &[&str]) -> String {
    let mut lines: Vec<String> = tokens
        .iter()
        .map(|t| format!(r#"data: {{"choices":[{{"delta":{{"content":"{t}"}}}}]}}"#))
        .collect();
    lines.push("data: [DONE]".to_string());
    lines.join("\n")
}

/// Send one `complete()` call through a mock server returning `response`.
async fn complete_with_mock(response: ResponseTemplate) -> anyhow::Result<ChatMessage> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(response)
        .mount(&server)
        .await;
    let provider = LlamafileProvider::new(Some(&server.uri()));
    provider
        .complete("You are helpful.", vec![ChatMessage::user("Hello")])
        .await
}

// ── complete(): single-shot, non-streaming ─────────────────────────

#[tokio::test]
async fn complete_returns_assistant_message_on_success() {
    let reply =
        complete_with_mock(ResponseTemplate::new(200).set_body_json(success_body("Hi there!")))
            .await
            .unwrap();

    assert_eq!(reply.role, Role::Assistant);
    assert_eq!(reply.content, "Hi there!");
}

#[tokio::test]
async fn complete_returns_error_on_non_200() {
    let err = complete_with_mock(ResponseTemplate::new(500).set_body_string("internal error"))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("500"), "unexpected error: {err}");
}

#[tokio::test]
async fn complete_returns_error_when_llamafile_offline() {
    // Port 1 is reserved — connection refused immediately.
    let provider = LlamafileProvider::new(Some("http://127.0.0.1:1"));
    let err = provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("llamafile"),
        "expected 'llamafile' in error, got: {err}"
    );
}

#[tokio::test]
async fn complete_model_name_is_llama_cpp() {
    let provider = LlamafileProvider::new(None);
    assert_eq!(provider.model_name(), DEFAULT_MODEL);
}

#[tokio::test]
async fn complete_system_prompt_is_first_message_in_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body("ok")))
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    provider
        .complete("my system prompt", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "expected exactly one HTTP request");

    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().expect("messages must be array");

    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "my system prompt");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "hi");
}

#[tokio::test]
async fn complete_multi_turn_conversation_preserves_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body("turn 2")))
        .mount(&server)
        .await;

    let history = vec![
        ChatMessage::user("What is 2+2?"),
        ChatMessage::assistant("It is 4."),
        ChatMessage::user("And 3+3?"),
    ];

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let reply = provider.complete("sys", history).await.unwrap();
    assert_eq!(reply.content, "turn 2");

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();

    // system + 3 conversation turns = 4 total
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "What is 2+2?");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "It is 4.");
    assert_eq!(messages[3]["role"], "user");
    assert_eq!(messages[3]["content"], "And 3+3?");
}

#[tokio::test]
async fn complete_request_body_includes_temperature_and_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body("ok")))
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()))
        .with_temperature(0.2)
        .with_max_tokens(512);

    provider
        .complete("sys", vec![ChatMessage::user("test")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();

    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["max_tokens"], 512);
}

#[tokio::test]
async fn complete_strips_stop_tokens_from_response() {
    let reply = complete_with_mock(
        ResponseTemplate::new(200).set_body_json(success_body("Hello there<end_of_turn>")),
    )
    .await
    .unwrap();

    assert_eq!(reply.content, "Hello there");
    assert!(!reply.content.contains("<end_of_turn>"));
}

#[tokio::test]
async fn complete_no_choices_returns_error() {
    let reply = complete_with_mock(
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"choices": []})),
    )
    .await;

    assert!(reply.is_err(), "empty choices should produce an error");
    assert!(
        reply.unwrap_err().to_string().contains("no choices"),
        "error should mention 'no choices'"
    );
}

// ── stream_complete(): token-by-token streaming ────────────────────

/// Collect the text tokens `stream_complete()` yields from `server`.
async fn stream_tokens(server: &MockServer, messages: Vec<ChatMessage>) -> Vec<String> {
    use pond_core::models::ports::provider::StreamToken;
    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete("system", messages);

    let mut tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(tok)) => tokens.push(tok),
            Ok(StreamToken::Usage(_)) => {}
            Err(e) => panic!("unexpected stream error: {e}"),
        }
    }
    tokens
}

#[tokio::test]
async fn stream_complete_returns_tokens_in_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["Hello", " world", "!"]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tokens = stream_tokens(&server, vec![ChatMessage::user("hi")]).await;

    assert!(!tokens.is_empty(), "expected tokens, got none");
    assert_eq!(tokens.join(""), "Hello world!");
}

#[tokio::test]
async fn stream_complete_request_body_has_stream_true() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["ok"]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("test")]);
    while stream.next().await.is_some() {}

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["stream"], true,
        "streaming request must set \"stream\": true"
    );
}

#[tokio::test]
async fn stream_complete_system_prompt_first_in_messages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["ok"]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete(
        "You are a helpful assistant.",
        vec![ChatMessage::user("hello")],
    );
    while stream.next().await.is_some() {}

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();

    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are a helpful assistant.");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "hello");
}

#[tokio::test]
async fn stream_complete_multi_turn_history_included() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["Because gravity."]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let history = vec![
        ChatMessage::user("why does the earth orbit the sun?"),
        ChatMessage::assistant("Because of gravity."),
        ChatMessage::user("explain in more detail"),
    ];

    let tokens = stream_tokens(&server, history).await;
    assert!(!tokens.is_empty());

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    // system + 3 turns = 4
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[3]["role"], "user");
    assert_eq!(messages[3]["content"], "explain in more detail");
}

#[tokio::test]
async fn stream_complete_propagates_error_on_non_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("hi")]);

    let first = stream.next().await.expect("should get an error item");
    assert!(first.is_err(), "expected error from non-200 response");
    assert!(
        first.unwrap_err().to_string().contains("503"),
        "error should mention status code"
    );
}

#[tokio::test]
async fn stream_complete_returns_error_when_offline() {
    let provider = LlamafileProvider::new(Some("http://127.0.0.1:1"));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("hi")]);

    let first = stream.next().await.expect("should get an error item");
    assert!(first.is_err());
    assert!(
        first.unwrap_err().to_string().contains("llamafile"),
        "error should mention 'llamafile'"
    );
}

#[tokio::test]
async fn stream_complete_skips_deltas_with_no_content_field() {
    let server = MockServer::start().await;

    let sse = [
        r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#, // no content → skip
        r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#,
        r#"data: {"choices":[{"delta":{}}]}"#, // empty delta → skip
        r#"data: {"choices":[{"delta":{"content":" there"}}]}"#,
        "data: [DONE]",
    ]
    .join("\n");

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse)
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tokens = stream_tokens(&server, vec![ChatMessage::user("hi")]).await;
    assert_eq!(tokens.join(""), "Hello there");
}

#[tokio::test]
async fn stream_complete_strips_stop_tokens_from_each_chunk() {
    let server = MockServer::start().await;

    let sse = [
        r#"data: {"choices":[{"delta":{"content":"The answer is 42"}}]}"#,
        r#"data: {"choices":[{"delta":{"content":"<end_of_turn>"}}]}"#,
        "data: [DONE]",
    ]
    .join("\n");

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse)
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let tokens = stream_tokens(&server, vec![ChatMessage::user("hi")]).await;

    assert!(
        !tokens.iter().any(|t| t.contains("<end_of_turn>")),
        "stop token should be stripped; got: {tokens:?}"
    );
    assert_eq!(tokens.join(""), "The answer is 42");
}

#[tokio::test]
async fn stream_complete_request_includes_temperature_and_max_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["ok"]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()))
        .with_temperature(0.1)
        .with_max_tokens(256);

    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("test")]);
    while stream.next().await.is_some() {}

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let temp = body["temperature"].as_f64().unwrap();
    assert!(
        (temp - 0.1).abs() < 1e-6,
        "temperature should be ~0.1, got {temp}"
    );
    assert_eq!(body["max_tokens"], 256);
    assert_eq!(body["stream"], true);
}

// ── Task role: tool-use prompts ────────────────────────────────────

#[tokio::test]
async fn task_role_complete_sends_correct_messages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(success_body("Reminder set for 9am.")),
        )
        .mount(&server)
        .await;

    let task_system_prompt = "You are an action-taking assistant. Execute tasks immediately.";
    let provider = LlamafileProvider::new(Some(&server.uri()));
    let reply = provider
        .complete(
            task_system_prompt,
            vec![ChatMessage::user("remind me to call mum at 9am")],
        )
        .await
        .unwrap();

    assert_eq!(reply.content, "Reminder set for 9am.");

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], task_system_prompt);
    assert_eq!(messages[1]["content"], "remind me to call mum at 9am");
}

#[tokio::test]
async fn task_role_stream_complete_yields_action_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["Scheduling", " reminder", " for 9am."]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete(
        "You are an action-taking assistant.",
        vec![ChatMessage::user("remind me to call mum at 9am")],
    );

    use pond_core::models::ports::provider::StreamToken;
    let mut tokens = Vec::new();
    while let Some(Ok(item)) = stream.next().await {
        if let StreamToken::Text(tok) = item {
            tokens.push(tok);
        }
    }

    assert_eq!(tokens.join(""), "Scheduling reminder for 9am.");
}

// ── Think role: long reasoning via stream_complete ─────────────────

#[tokio::test]
async fn think_role_stream_complete_handles_long_reasoning_response() {
    let server = MockServer::start().await;
    let reasoning_tokens = [
        "First, ",
        "consider the ",
        "gravitational ",
        "constant. ",
        "Then, ",
        "apply ",
        "Newton's ",
        "second law.",
    ];
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&reasoning_tokens))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete(
        "You are a deep-thinking assistant.",
        vec![ChatMessage::user("explain why objects fall")],
    );

    use pond_core::models::ports::provider::StreamToken;
    let mut tokens = Vec::new();
    while let Some(Ok(item)) = stream.next().await {
        if let StreamToken::Text(tok) = item {
            tokens.push(tok);
        }
    }

    let full = tokens.join("");
    assert!(
        full.contains("gravitational") && full.contains("Newton"),
        "expected reasoning tokens, got: {full}"
    );
    assert_eq!(tokens.len(), reasoning_tokens.len());
}

#[tokio::test]
async fn think_role_complete_multi_turn_reasoning() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body(
            "Building on my previous point: entropy increases.",
        )))
        .mount(&server)
        .await;

    let history = vec![
        ChatMessage::user("explain thermodynamics step by step"),
        ChatMessage::assistant("Step 1: Energy cannot be created or destroyed."),
        ChatMessage::user("continue"),
    ];

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let reply = provider
        .complete("You are a thoughtful explainer.", history)
        .await
        .unwrap();

    assert!(reply.content.contains("entropy"));

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4); // system + 3 turns
    assert_eq!(
        messages[2]["content"],
        "Step 1: Energy cannot be created or destroyed."
    );
    assert_eq!(messages[3]["content"], "continue");
}

// ── Token usage ────────────────────────────────────────────────────

/// `usage` rides on llamafile's final (`finish_reason: "stop"`) chunk and must come out last.
#[tokio::test]
async fn stream_complete_parses_usage_from_final_chunk() {
    use pond_core::models::ports::provider::StreamToken;

    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"stop\"}],",
        "\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":47,\"total_tokens\":59}}\n\n",
        "data: [DONE]\n\n"
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("hi")]);

    let mut texts = Vec::new();
    let mut usage_opt = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t)) => texts.push(t),
            Ok(StreamToken::Usage(u)) => usage_opt = Some(u),
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    assert_eq!(texts, vec!["Hello".to_string()], "expected one text token");
    let usage = usage_opt.expect("expected a Usage token after the stream");
    assert_eq!(usage.prompt_tokens, 12, "prompt_tokens mismatch");
    assert_eq!(usage.completion_tokens, 47, "completion_tokens mismatch");
}

#[tokio::test]
async fn stream_complete_no_usage_when_chunk_omits_it() {
    use pond_core::models::ports::provider::StreamToken;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body(&["Hello", " world"]))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    let mut stream = provider.stream_complete("sys", vec![ChatMessage::user("hi")]);

    let mut has_usage = false;
    while let Some(Ok(item)) = stream.next().await {
        if let StreamToken::Usage(_) = item {
            has_usage = true;
        }
    }

    assert!(
        !has_usage,
        "expected no Usage token when the SSE body omits usage"
    );
}
