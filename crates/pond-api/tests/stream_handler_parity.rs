//! The two chat stream handlers agree about persistence, extraction and scope.
//! Ordering is invisible in a drained SSE body, so half of these checks read `routes.rs`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use serde_json::{json, Value};
use tower::ServiceExt;

const ROUTES: &str = include_str!("../src/routes.rs");

/// The two streaming bodies; chat's turn lives in `drive_turn`, not `chat_stream_inner`.
const CHAT: &str = "drive_turn";
const AGENT: &str = "agent_chat_stream";

/// The event-to-SSE translator: a `TurnAccumulator` method, so sliced by [`method_body`].
const TRANSLATOR: &str = "absorb";

/// `routes.rs` minus its test module, whose `AgentStreamEvent` fixtures would skew the counts.
fn production() -> &'static str {
    let code = ROUTES
        .split_once("#[cfg(test)]")
        .map(|(before, _)| before)
        .unwrap_or_else(|| panic!("routes.rs has no test module -- did the file move?"));
    // A `#[cfg(test)]` above the handlers would truncate this and make absence checks vacuous.
    for needle in [
        "fn chat_stream_inner(",
        "async fn drive_turn(",
        "async fn agent_chat_stream(",
        "fn absorb(",
    ] {
        assert!(
            code.contains(needle),
            "the production slice of routes.rs no longer contains `{needle}` -- \
             a `#[cfg(test)]` item now sits above it, so this file is measuring \
             a preamble and its absence assertions mean nothing"
        );
    }
    code
}

/// A function's source up to the next top-level item; panics on a miss rather than go vacuous.
fn handler_body(name: &str) -> &'static str {
    let src = production();
    let (sig, start) = [format!("async fn {name}("), format!("fn {name}(")]
        .into_iter()
        .find_map(|sig| src.find(&sig).map(|at| (sig, at)))
        .unwrap_or_else(|| panic!("{name} is gone from routes.rs -- this guard needs rewriting"));
    let rest = &src[start + sig.len()..];
    let end = rest
        .find("\nasync fn ")
        .into_iter()
        .chain(rest.find("\nfn "))
        .chain(rest.find("\npub async fn "))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// An indented method's source, ending at `\n    }` where rustfmt puts its closing brace.
fn method_body(name: &str) -> &'static str {
    let src = production();
    let sig = format!("fn {name}(");
    let start = src
        .find(&sig)
        .unwrap_or_else(|| panic!("{name} is gone from routes.rs -- this guard needs rewriting"));
    let rest = &src[start + sig.len()..];
    let end = rest.find("\n    }").unwrap_or(rest.len());
    &rest[..end]
}

/// Drops `//` comments so the counts below count code, not mentions in comments.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(at) => &l[..at],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `body` uses `word` as a whole identifier, so `Status` doesn't match `StatusCode`.
fn mentions_identifier(body: &str, word: &str) -> bool {
    let is_ident = |c: Option<char>| matches!(c, Some(c) if c.is_alphanumeric() || c == '_');
    body.match_indices(word).any(|(at, _)| {
        !is_ident(body[..at].chars().next_back())
            && !is_ident(body[at + word.len()..].chars().next())
    })
}

#[test]
fn the_identifier_search_can_tell_a_name_from_a_longer_one() {
    assert!(
        mentions_identifier("match ev { Ev::Status { content } => {}", "Status"),
        "the search misses a variant name that IS there, so every assertion \
         built on it is vacuous"
    );
    assert!(
        !mentions_identifier("return StatusCode::BAD_REQUEST.into_response();", "Status"),
        "the search reports a variant name inside `StatusCode`, so it would fail \
         this file for a handler doing ordinary HTTP"
    );
    assert!(
        !mentions_identifier("let text_content = body.text;", "Text"),
        "the search is not case-sensitive or not boundary-aware"
    );
}

fn position(body: &str, needle: &str, handler: &str) -> usize {
    body.find(needle).unwrap_or_else(|| {
        panic!("{handler} no longer contains `{needle}` -- PAI-5 P7 parity has regressed")
    })
}

/// The plain `persist_assistant_turn` silently skips memory extraction.
#[test]
fn both_stream_handlers_extract_memory_from_the_turn() {
    for handler in [CHAT, AGENT] {
        let body = handler_body(handler);
        assert!(
            body.contains("persist_assistant_turn_with_extraction"),
            "{handler} persists its turn without extracting from it, so a conversation held \
             there contributes nothing to memory"
        );
        assert!(
            body.contains("with_memory_extraction"),
            "{handler} never wires the extractor onto its ChatService, so \
             persist_assistant_turn_with_extraction has nothing to spawn"
        );
    }
}

/// Vacuity control for `handler_body`, which every source guard here relies on.
#[test]
fn the_two_handler_bodies_are_really_two_different_handlers() {
    let chat = handler_body(CHAT);
    let agent = handler_body(AGENT);
    assert_ne!(
        chat, agent,
        "the slicer returned the same text for both handlers"
    );
    for (name, body) in [(CHAT, chat), (AGENT, agent)] {
        assert!(
            body.len() > 2_000,
            "{name}'s body came out at {} bytes, which is too small to be the handler -- the \
             slicer is broken and every assertion in this file is vacuous",
            body.len()
        );
        assert!(
            body.len() < ROUTES.len() / 2,
            "{name}'s body came out at {} bytes of a {}-byte file -- the slicer ran past the \
             end of the handler",
            body.len(),
            ROUTES.len()
        );
    }
    assert!(
        agent.contains("model_role: \"task\""),
        "the body claimed to be agent_chat_stream does not look like it"
    );
}

/// Resolving scope after building `ChatService` attributes extraction to `Household`.
#[test]
fn agent_chat_stream_knows_who_is_speaking_before_it_builds_the_service() {
    let body = handler_body(AGENT);
    let resolved = position(body, "let turn_scope = resolve_turn_scope(", AGENT);
    let built = position(body, "ChatService::new(", AGENT);
    let scoped = position(body, ".with_profile_scope(turn_scope", AGENT);

    assert!(
        resolved < built,
        "the turn's scope is resolved AFTER the ChatService is built, so the service keeps its \
         Household default and every memory extracted from this route is attributed to the whole \
         household -- whoever was speaking"
    );
    assert!(
        scoped > built,
        "with_profile_scope is not applied to the constructed service"
    );
}

/// Two exhaustive matches over a growing enum means writing each new variant twice.
#[test]
fn the_engine_event_match_lives_in_exactly_one_place() {
    // Names unlikely in a handler otherwise; `Status`/`Text`/`Done`/`Error` would false-positive.
    const DISTINCTIVE_VARIANTS: [&str; 7] = [
        "Thinking",
        "ToolCall",
        "ToolResult",
        "ReviewStatus",
        "ReviewRevision",
        "TurnLimitReached",
        "SubagentProgress",
    ];

    let translator = strip_line_comments(method_body(TRANSLATOR));
    let inside = translator.matches("AgentStreamEvent::").count();

    // Vacuity control: if the slicer or name rots, every absence check below passes trivially.
    assert!(
        inside >= 10,
        "`{TRANSLATOR}` matches only {inside} `AgentStreamEvent::` variants. The \
         enum has at least ten, so either this guard is pointing at the wrong \
         function or the translator has stopped being the translator -- and \
         every other assertion in this test is now vacuous"
    );
    assert!(
        translator.len() > 1_000 && translator.len() < ROUTES.len() / 10,
        "`{TRANSLATOR}` sliced to {} bytes of a {}-byte file, which is not the \
         shape of one method. Too small and the slicer stopped early; too large \
         and it ran past the end and is quoting somebody else's code as proof",
        translator.len(),
        ROUTES.len()
    );
    // Positive control on real code: a `mentions_identifier` that finds nothing fails here.
    for variant in DISTINCTIVE_VARIANTS {
        assert!(
            mentions_identifier(&translator, variant),
            "`{TRANSLATOR}` no longer handles `{variant}`. Either the enum was \
             renamed -- in which case this list needs the new spelling before it \
             can detect a second copy again -- or the translator has stopped \
             being exhaustive"
        );
    }

    for handler in [CHAT, AGENT] {
        let body = strip_line_comments(handler_body(handler));

        // Absence checks can't see a handler that stopped folding events at all.
        assert!(
            body.contains("turn.absorb(event)"),
            "{handler} no longer folds the engine's events through \
             `{TRANSLATOR}`. Every frame that route emits now comes from \
             somewhere this file cannot see, and the single-answer property P7 \
             bought is gone whether or not a second match is visible below"
        );

        assert!(
            !body.contains("AgentStreamEvent::"),
            "{handler} matches engine events itself instead of folding them \
             through `{TRANSLATOR}`. Two copies of the match is what PAI-5 P7 \
             removed: the next variant gets written into one of them, the other \
             silently drops it, and the two routes answer the same engine \
             differently"
        );
        // Neither spelling matches the handler's own `match event_result {`.
        for spelling in ["match &event", "match event {"] {
            assert!(
                !body.contains(spelling),
                "{handler} contains `{spelling}` -- it is matching the engine \
                 event itself. Importing the enum under another name hides it \
                 from the assertion above but not from this one"
            );
        }
        for variant in DISTINCTIVE_VARIANTS {
            assert!(
                !mentions_identifier(&body, variant),
                "{handler} names the engine variant `{variant}`, so it is \
                 destructuring engine events somewhere of its own. A second \
                 fold cannot avoid spelling the variants, whatever the enum \
                 itself is imported as"
            );
        }
    }

    // Nor anywhere else in `routes.rs`: a third stream path would drift from both.
    let everywhere = strip_line_comments(production())
        .matches("AgentStreamEvent::")
        .count();
    assert_eq!(
        everywhere, inside,
        "routes.rs matches `AgentStreamEvent::` in {everywhere} places but only \
         {inside} of them are in `{TRANSLATOR}`. Every frame shape belongs to \
         one function so that a new variant is written once"
    );
}

/// A wrong `with_profile_scope` argument misattributes silently; nothing else catches it.
#[test]
fn neither_handler_scopes_its_service_from_a_literal() {
    for handler in [CHAT, AGENT] {
        let body = handler_body(handler);
        assert!(
            body.contains(".with_profile_scope(turn_scope"),
            "{handler} does not scope its ChatService from the turn's resolved scope"
        );
        assert!(
            !body.contains(".with_profile_scope(ProfileScope::"),
            "{handler} scopes its ChatService from a literal, which cannot be the speaker"
        );
    }
}

/// The desktop reloads the session on stream close, so an early `done` loses the answer.
#[test]
fn chat_stream_persists_the_turn_before_it_closes_the_stream() {
    let body = handler_body(CHAT);
    let persisted = position(body, "persist_assistant_turn_with_extraction", CHAT);
    let done = position(body, "\"done\": true", CHAT);

    assert!(
        done > persisted,
        "{CHAT} yields its `done` frame at byte {done} of its body but only persists the \
         turn at byte {persisted}. A client that reloads the session when the stream closes \
         reads a conversation missing the answer it just watched arrive"
    );
    assert_eq!(
        body.matches("\"done\": true").count(),
        1,
        "{CHAT} builds more than one `done` frame, so the ordering asserted above is only \
         true of whichever one this guard happened to find first"
    );
}

// ── The other half: the route, driven ────────────────────────────────────────

/// Always-completed onboarding, so its gate never blocks the route under test.
struct CompletedOnboarding;

#[async_trait::async_trait]
impl OnboardingRepository for CompletedOnboarding {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct StubDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for StubDeviceRegistry {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "stub".to_string(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".to_string(),
            last_seen: None,
            is_online: false,
            room: req.room,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(StubDeviceRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

/// POSTs one turn and drains the SSE body; persistence runs only while the generator is polled.
async fn drive_agent_stream(app: &axum::Router, session_id: &str, message: &str) -> Vec<Value> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/agent/chat/stream")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "session_id": session_id,
                        "message": message,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "/agent/chat/stream refused the turn, so nothing below is about the stream"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).expect("an SSE body is UTF-8");
    let frames: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| {
            serde_json::from_str(data).unwrap_or_else(|e| {
                panic!("a frame the browser has to parse is not JSON: {data} -- {e}")
            })
        })
        .collect();

    // An early bail-out still returns 200, with a single `error` frame.
    assert!(
        !frames.iter().any(|f| f.get("error").is_some()),
        "the route reported an error instead of running the turn: {frames:?}"
    );
    frames
}

/// The desktop's reader loop ends only on `{"done": true}`.
#[tokio::test]
async fn the_agent_route_ends_the_turn_it_opened() {
    let (app, _tmp) = make_app().await;
    let session_id = "agent-stream-terminates";

    let frames = drive_agent_stream(&app, session_id, "hello").await;

    let done: Vec<&Value> = frames.iter().filter(|f| f.get("done").is_some()).collect();
    assert_eq!(
        done.len(),
        1,
        "/agent/chat/stream must end its stream with exactly one `done` frame. None and a \
         browser reads forever; two and it closes a turn that is still arriving. Frames \
         were: {frames:?}"
    );
    assert_eq!(
        done[0]["done"],
        json!(true),
        "the terminating frame must say `done: true`, not merely carry the key: {:?}",
        done[0]
    );
    assert_eq!(
        done[0]["session_id"],
        json!(session_id),
        "the `done` frame carries the session the client reloads when the stream ends: {:?}",
        done[0]
    );

    // The turn really answered, so the `done` above isn't sent over silence.
    let answer: String = frames
        .iter()
        .filter(|f| f["type"] == "text")
        .filter_map(|f| f["content"].as_str())
        .collect();
    assert_eq!(
        answer, "Echo: hello",
        "the answer the client assembles from the text frames is not the answer the engine \
         produced. Frames were: {frames:?}"
    );
}

/// Proves the persistence call actually runs; one in an unpolled generator stores nothing.
#[tokio::test]
async fn the_agent_route_persists_the_turn_it_streamed() {
    let (app, _tmp) = make_app().await;
    let session_id = "agent-stream-persists";

    drive_agent_stream(&app, session_id, "hello").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/sessions/{session_id}/messages"))
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    let messages = json["messages"].as_array().expect("messages array");

    assert_eq!(
        messages.len(),
        2,
        "expected the user's turn and the assistant's answer, got: {json}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hello");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["content"], "Echo: hello",
        "the answer persisted is not the answer streamed, so the next turn's history is \
         not the conversation the user had"
    );
}
