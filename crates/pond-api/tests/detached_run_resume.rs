//! Turns that outlive their connection, through the real router. `SlowAgent` is local because
//! ~30 suites depend on the shared `MockAgent` yielding with no delay.

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
use tower::ServiceExt;

// ── Minimal stubs ──────────────────────────────────────────────────────────────

/// Onboarding always reports completed so the gate never blocks chat.
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
    // Answering "not onboarded" would make every onboarding write route public.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct MockDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for MockDeviceRegistry {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock".to_string(),
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

async fn make_app_with(
    agent: Arc<dyn pond_core::models::ports::agent::Agent>,
    runs: pond_api::runs::RunSupervisor,
) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();

    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        account_sync: None,
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent,
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
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
        runs: Arc::new(runs),
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

// ── An agent that takes its time ──────────────────────────────────────────────

use futures::StreamExt;
use pond_core::models::ports::agent::{Agent, AgentRequest, AgentResponse, AgentStreamEvent};
use std::time::Duration;

/// Emits `chunks` text events spaced by `gap`, then `Done`.
struct SlowAgent {
    chunks: Vec<String>,
    gap: Duration,
    /// Says nothing at all before finishing. For the orphan-repair case.
    silent: bool,
    /// Quiet time before `Done`, so "cancel mid-turn" is a window, not a race (SSE frames batch).
    tail: Duration,
}

impl SlowAgent {
    fn new(chunks: &[&str], gap_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            chunks: chunks.iter().map(|s| s.to_string()).collect(),
            gap: Duration::from_millis(gap_ms),
            silent: false,
            tail: Duration::ZERO,
        })
    }

    /// Say `chunks`, then hold the turn open for `tail_ms` before finishing.
    fn with_tail(chunks: &[&str], gap_ms: u64, tail_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            chunks: chunks.iter().map(|s| s.to_string()).collect(),
            gap: Duration::from_millis(gap_ms),
            silent: false,
            tail: Duration::from_millis(tail_ms),
        })
    }
    /// Says nothing at all for `gap`, then finishes. For the orphan-repair case.
    fn silent(gap_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            chunks: vec![],
            gap: Duration::from_millis(gap_ms),
            silent: true,
            tail: Duration::ZERO,
        })
    }
}

#[async_trait::async_trait]
impl Agent for SlowAgent {
    async fn chat(&self, _request: AgentRequest) -> anyhow::Result<AgentResponse> {
        unreachable!("these tests only stream")
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<AgentStreamEvent>>> {
        let chunks = self.chunks.clone();
        let gap = self.gap;
        let silent = self.silent;
        let tail = self.tail;
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let stream = async_stream::stream! {
            if silent {
                tokio::time::sleep(gap).await;
            }
            for chunk in chunks {
                tokio::time::sleep(gap).await;
                yield Ok(AgentStreamEvent::Text { content: chunk });
            }
            tokio::time::sleep(tail).await;
            yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: None, stats: None });
        };
        Ok(stream.boxed())
    }

    async fn call_tool(&self, _s: &str, _n: &str, _a: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn make_app(agent: Arc<dyn Agent>) -> (axum::Router, tempfile::TempDir) {
    make_app_with(agent, pond_api::runs::RunSupervisor::default()).await
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

/// Drain an SSE body to the end, returning its `data:` payloads.
async fn drain(body: Body) -> Vec<String> {
    let mut out = Vec::new();
    let mut stream = body.into_data_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        for line in String::from_utf8_lossy(&chunk).lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                out.push(rest.to_string());
            }
        }
    }
    out
}

/// Read the first `n` payloads, then drop the body: the reader walking away mid-turn.
async fn read_then_drop(body: Body, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stream = body.into_data_stream();
    while out.len() < n {
        match stream.next().await {
            Some(Ok(chunk)) => {
                for line in String::from_utf8_lossy(&chunk).lines() {
                    if let Some(rest) = line.strip_prefix("data: ") {
                        out.push(rest.to_string());
                    }
                }
            }
            _ => break,
        }
    }
    drop(stream);
    out
}

fn parse(payloads: &[String]) -> Vec<serde_json::Value> {
    payloads
        .iter()
        .filter_map(|p| serde_json::from_str(p).ok())
        .collect()
}

fn field<'a>(frames: &'a [serde_json::Value], ty: &str) -> Option<&'a serde_json::Value> {
    frames
        .iter()
        .find(|f| f.get("type").and_then(|t| t.as_str()) == Some(ty))
}

async fn json_body(body: Body) -> serde_json::Value {
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

async fn assistant_text(app: &axum::Router, session_id: &str) -> Vec<String> {
    let res = app
        .clone()
        .oneshot(get(&format!("/api/v1/sessions/{session_id}/messages")))
        .await
        .unwrap();
    let v = json_body(res.into_body()).await;
    v.get("messages")
        .and_then(|m| m.as_array())
        .map(|rows| {
            rows.iter()
                .filter(|r| r.get("role").and_then(|x| x.as_str()) == Some("assistant"))
                .filter_map(|r| r.get("content").and_then(|c| c.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Poll the discovery route until the run reaches a terminal state.
async fn await_terminal(app: &axum::Router, session_id: &str) -> serde_json::Value {
    for _ in 0..100 {
        let res = app
            .clone()
            .oneshot(get(&format!("/api/v1/sessions/{session_id}/active-run")))
            .await
            .unwrap();
        let v = json_body(res.into_body()).await;
        if v.get("state")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s != "running")
        {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run never reached a terminal state");
}

// ── Resuming a detached turn ──────────────────────────────────────────────────

#[tokio::test]
async fn a_resumable_turn_finishes_after_its_reader_walks_away() {
    let (app, _tmp) = make_app(SlowAgent::new(&["one ", "two ", "three"], 40)).await;
    let session = "sess-detach";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hello", "resumable": true}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Take the opening frames, then hang up mid-answer.
    let seen = read_then_drop(res.into_body(), 2).await;
    let started = parse(&seen);
    assert!(
        field(&started, "run_started").is_some(),
        "a resumable turn must name itself so a client can reconnect to it"
    );

    let final_state = await_terminal(&app, session).await;
    assert_eq!(
        final_state.get("state").unwrap().as_str().unwrap(),
        "finished",
        "the turn belongs to the task now, not to whoever was reading it"
    );
    assert_eq!(
        assistant_text(&app, session).await,
        vec!["one two three".to_string()],
        "and it wrote the whole answer down while nobody was watching"
    );
}

#[tokio::test]
async fn reattaching_replays_what_was_missed_and_then_the_ending() {
    let (app, _tmp) = make_app(SlowAgent::new(&["alpha ", "beta ", "gamma"], 40)).await;
    let session = "sess-replay";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    let seen = read_then_drop(res.into_body(), 2).await;
    let run_id = parse(&seen)
        .iter()
        .find_map(|f| f.get("run_id").and_then(|r| r.as_str()).map(String::from))
        .expect("run_started carries the id to come back to");

    await_terminal(&app, session).await;

    let res = app
        .clone()
        .oneshot(get(&format!(
            "/api/v1/chat/runs/{run_id}/events?after_seq=0"
        )))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let frames = parse(&drain(res.into_body()).await);

    assert!(
        field(&frames, "reattached").is_some(),
        "a reattach says so, so the client can tell a replay from a fresh turn"
    );
    assert!(
        field(&frames, "replay_gap").is_none(),
        "nothing was evicted, so nothing was lost"
    );
    let text: String = frames
        .iter()
        .filter(|f| f.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|f| f.get("content").and_then(|c| c.as_str()))
        .collect();
    assert_eq!(text, "alpha beta gamma");
    assert_eq!(
        frames.iter().filter(|f| f.get("done").is_some()).count(),
        1,
        "exactly one terminal frame, or a client waits forever or ends twice"
    );
}

#[tokio::test]
async fn a_client_that_only_knows_its_session_can_find_the_run() {
    let (app, _tmp) = make_app(SlowAgent::new(&["x"], 40)).await;
    let session = "sess-discover";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    let seen = read_then_drop(res.into_body(), 1).await;
    let run_id = parse(&seen)[0]["run_id"].as_str().unwrap().to_string();

    // A restarted app knows only the session and must be handed the run id to reattach.
    let res = app
        .clone()
        .oneshot(get(&format!("/api/v1/sessions/{session}/active-run")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res.into_body()).await;
    assert_eq!(v["run_id"].as_str().unwrap(), run_id);
    assert!(v["epoch"].as_str().is_some());
}

// ── The contract that must NOT change ─────────────────────────────────────────

#[tokio::test]
async fn a_turn_that_did_not_ask_to_be_resumable_still_dies_with_its_reader() {
    // Voice's speculative pre-fire depends on this: never persist an answer to a half-sentence.
    let (app, _tmp) = make_app(SlowAgent::new(&["never ", "arrives"], 60)).await;
    let session = "sess-ephemeral";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi"}),
        ))
        .await
        .unwrap();
    drop(res.into_body());

    tokio::time::sleep(Duration::from_millis(400)).await;

    let res = app
        .clone()
        .oneshot(get(&format!("/api/v1/sessions/{session}/active-run")))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "an ephemeral run is not registered: there is nothing to come back to"
    );
    let text = assistant_text(&app, session).await;
    assert!(
        text.iter().all(|t| t != "never arrives"),
        "the turn was abandoned, so it must not have run to completion"
    );
}

// ── Stopping on purpose ───────────────────────────────────────────────────────

const SPOKEN: &str = "The pond keeps its own counsel about most things, but when \
asked directly it will tell you that the geese arrived on a Tuesday, that the \
water was higher that year than anyone remembered, and that nobody thought to \
write any of it down until much later, which is how most records begin.";

#[tokio::test]
async fn cancelling_keeps_what_was_already_said() {
    let (app, _tmp) = make_app(SlowAgent::with_tail(&[SPOKEN], 50, 3_000)).await;
    let session = "sess-cancel";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    // Cancel on elapsed time, inside the 3s quiet window after the agent speaks at 50ms.
    let seen = read_then_drop(res.into_body(), 1).await;
    let run_id = parse(&seen)[0]["run_id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let res = app
        .clone()
        .oneshot(post(
            &format!("/api/v1/chat/runs/{run_id}/cancel"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = await_terminal(&app, session).await;
    assert_eq!(v["state"].as_str().unwrap(), "cancelled");

    let text = assistant_text(&app, session).await;
    assert_eq!(
        text.len(),
        1,
        "a cancelled turn still writes down what it had already said -- a \
         barge-in should not erase the sentence the user just heard"
    );
    assert!(text[0].starts_with("The pond"), "got: {:?}", text[0]);
}

#[tokio::test]
async fn cancelling_a_run_that_never_spoke_removes_the_orphaned_question() {
    let (app, _tmp) = make_app(SlowAgent::silent(2_000)).await;
    let session = "sess-orphan";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "unanswered", "resumable": true}),
        ))
        .await
        .unwrap();
    let seen = read_then_drop(res.into_body(), 1).await;
    let run_id = parse(&seen)[0]["run_id"].as_str().unwrap().to_string();

    let _ = app
        .clone()
        .oneshot(post(
            &format!("/api/v1/chat/runs/{run_id}/cancel"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    await_terminal(&app, session).await;

    let res = app
        .clone()
        .oneshot(get(&format!("/api/v1/sessions/{session}/messages")))
        .await
        .unwrap();
    let v = json_body(res.into_body()).await;
    let rows = v["messages"].as_array().cloned().unwrap_or_default();
    assert!(
        rows.is_empty(),
        "the user's message was stored before inference began and nothing ever \
         answered it; leaving it shows a question with no reply and feeds the \
         next turn a prompt the pond never responded to: {rows:?}"
    );
}

#[tokio::test]
async fn stopping_by_session_is_the_same_stop() {
    let (app, _tmp) = make_app(SlowAgent::with_tail(&["a"], 50, 3_000)).await;
    let session = "sess-stop-by-session";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    let _ = read_then_drop(res.into_body(), 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let res = app
        .clone()
        .oneshot(delete(&format!("/api/v1/sessions/{session}/active-run")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = await_terminal(&app, session).await;
    assert_eq!(v["state"].as_str().unwrap(), "cancelled");
}

// ── Being honest about what is gone ───────────────────────────────────────────

#[tokio::test]
async fn a_run_from_a_previous_process_is_told_so_plainly() {
    let (app, _tmp) = make_app(SlowAgent::new(&["x"], 20)).await;
    let session = "sess-epoch";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    let seen = read_then_drop(res.into_body(), 1).await;
    let run_id = parse(&seen)[0]["run_id"].as_str().unwrap().to_string();

    let res = app
        .clone()
        .oneshot(get(&format!(
            "/api/v1/chat/runs/{run_id}/events?epoch=from-a-server-that-died"
        )))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::GONE,
        "410 and a reason, not a 404 the client cannot tell apart from \"it aged out\""
    );
    let v = json_body(res.into_body()).await;
    assert_eq!(v["reason"].as_str().unwrap(), "server_restarted");
    assert_eq!(v["advice"].as_str().unwrap(), "reload_session_messages");
}

#[tokio::test]
async fn a_finished_run_stops_being_reattachable_once_its_retention_passes() {
    let (app, _tmp) = make_app_with(
        SlowAgent::new(&["x"], 10),
        pond_api::runs::RunSupervisor::new(4, Duration::ZERO),
    )
    .await;
    let session = "sess-ttl";

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": session, "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    let seen = drain(res.into_body()).await;
    let run_id = parse(&seen)[0]["run_id"].as_str().unwrap().to_string();

    let res = app
        .clone()
        .oneshot(get(&format!("/api/v1/chat/runs/{run_id}/events")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_detached_run_cap_refuses_in_its_own_words() {
    let (app, _tmp) = make_app_with(
        SlowAgent::new(&["x"], 400),
        pond_api::runs::RunSupervisor::new(1, Duration::from_secs(60)),
    )
    .await;

    let first = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": "cap-1", "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    let _held = read_then_drop(first.into_body(), 1).await;

    let second = app
        .clone()
        .oneshot(post(
            "/api/v1/chat/stream",
            serde_json::json!({"session_id": "cap-2", "message": "hi", "resumable": true}),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = json_body(second.into_body()).await;
    assert_eq!(
        v["kind"].as_str().unwrap(),
        "run_cap",
        "distinguishable from the interactive stream cap, which says something \
         else entirely and is fixed by a different action"
    );
}
