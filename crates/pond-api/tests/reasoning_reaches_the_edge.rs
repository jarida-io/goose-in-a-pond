//! PAI-5 P2's tail: the reasoning count reaches a client. Asserts that
//! `reasoning_tokens` rides the `turn_stats` frame on both stream routes and
//! before `done`, sits alongside `completion_tokens` rather than inside it, and
//! that `/usage/summary` reports counted turns so `null` can be told from `0`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::agent::{Agent, AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::models::ports::provider::UsageStats;
use pond_core::shared::domain::turn_stats::TurnStats;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use serde_json::Value;
use tower::ServiceExt;

// ── Stubs ────────────────────────────────────────────────────────────────────

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

struct NoDevices;

#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
        anyhow::bail!("not used")
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

/// An engine that finishes a turn with the numbers `GooseAdapter` reports.
///
/// `reasoning: None` is the provider that counted nothing — the case a
/// `unwrap_or(0)` anywhere on the way to the client would erase.
struct StatsAgent {
    reasoning: Option<u32>,
    /// Attempts beyond the first this turn needed. Not an `Option`: every turn
    /// that reaches the handler was observed, so 0 is a fact and not a gap.
    reengagements: u32,
}

#[async_trait::async_trait]
impl Agent for StatsAgent {
    async fn chat(&self, request: AgentRequest) -> anyhow::Result<AgentResponse> {
        Ok(AgentResponse {
            text: format!("Echo: {}", request.message),
            metadata: Default::default(),
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<AgentStreamEvent>>> {
        use futures::StreamExt;
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let reasoning = self.reasoning;
        let reengagements = self.reengagements;
        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::Text { content: "Considered answer".to_string() });
            let mut stats = TurnStats {
                ttft_ms: Some(412),
                prefill_ms: Some(2000),
                decode_ms: Some(4000),
                prompt_tokens: 1000,
                completion_tokens: 88,
                reasoning_tokens: reasoning,
                context_used_tokens: Some(1000),
                context_limit_tokens: Some(3072),
                inference_count: 1,
                reengagements,
                ..Default::default()
            };
            stats.finalize_rates();
            yield Ok(AgentStreamEvent::Done {
                session_id,
                model_role,
                usage: Some(UsageStats {
                    prompt_tokens: 1000,
                    completion_tokens: 88,
                    reasoning_tokens: reasoning,
                }),
                stats: Some(stats),
            });
        };
        Ok(stream.boxed())
    }
}

struct Harness {
    app: axum::Router,
    storage: Arc<SqliteSessionStorage>,
    _tmp: tempfile::TempDir,
}

async fn make_app(agent: Arc<dyn Agent>) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let hs = MockHandshake::new();
    hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage: storage.clone(),
        http_client: reqwest::Client::new(),
        agent,
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        lane: None,
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
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        extraction_status: None,
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
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
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

    Harness {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        storage,
        _tmp: tmp,
    }
}

/// Drive one of the two stream routes and return its frames in order.
async fn drive(app: &axum::Router, route: &str, session_id: &str) -> Vec<Value> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(route)
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "session_id": session_id,
                        "message": "how long did you think about that?"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{route} did not answer");

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes).to_string();
    let frames: Vec<Value> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        !frames.is_empty(),
        "{route} produced no parseable SSE frames; raw body: {body}"
    );
    frames
}

fn turn_stats_of(frames: &[Value], route: &str) -> Value {
    frames
        .iter()
        .find(|f| f["type"] == "turn_stats")
        .unwrap_or_else(|| panic!("{route} emitted no turn_stats frame; frames: {frames:?}"))
        .clone()
}

/// The two routes, so every assertion below is quantified over both rather than
/// written twice. `/chat/stream` had the frame and `/agent/chat/stream` did not,
/// which is precisely the drift a per-route test cannot see.
const STREAM_ROUTES: [&str; 2] = ["/api/v1/chat/stream", "/api/v1/agent/chat/stream"];

// ── The frame ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn both_stream_routes_report_the_reasoning_count_beside_the_completion_count() {
    for (i, route) in STREAM_ROUTES.iter().enumerate() {
        let h = make_app(Arc::new(StatsAgent {
            reasoning: Some(240),
            reengagements: 0,
        }))
        .await;
        let frames = drive(&h.app, route, &format!("reasoning-{i}")).await;
        let stats = turn_stats_of(&frames, route);

        assert_eq!(
            stats["reasoning_tokens"], 240,
            "{route} dropped the reasoning count. It is produced by \
             `count_reasoning_tokens`, it rides `AgentStreamEvent::Done`, and this \
             frame is the only place a client can see it. Frame: {stats}"
        );
        // The alongside rule. If a future change ever subtracts the estimate
        // from the engine's own number, this is where it shows up.
        assert_eq!(
            stats["completion_tokens"], 88,
            "the reasoning estimate moved the engine-reported completion count. \
             GIAP counts the thinking text itself; deducting it corrupts the one \
             number that was actually measured. Frame: {stats}"
        );
        assert_eq!(
            stats["prompt_tokens"], 1000,
            "reasoning must not touch the prompt count either. Frame: {stats}"
        );
        assert_eq!(
            stats["decode_tok_per_sec"], 22.0,
            "`finalize_rates` deliberately ignores reasoning, so the decode rate \
             stays a rate over what the engine reported. Frame: {stats}"
        );
    }
}

/// `None` and `Some(0)` are different facts, and only one of them is "this turn
/// did no thinking".
#[tokio::test]
async fn a_turn_nobody_counted_reads_as_null_and_not_as_zero() {
    for (i, route) in STREAM_ROUTES.iter().enumerate() {
        let h = make_app(Arc::new(StatsAgent {
            reasoning: None,
            reengagements: 0,
        }))
        .await;
        let frames = drive(&h.app, route, &format!("uncounted-{i}")).await;
        let stats = turn_stats_of(&frames, route);

        // The key must be PRESENT and null, not missing. `Value::index` returns
        // `Null` for an absent key, so `is_null()` alone is equally satisfied by a
        // frame that dropped the field altogether, which is the regression the
        // test above exists for.
        let reported = stats
            .as_object()
            .and_then(|o| o.get("reasoning_tokens"))
            .unwrap_or_else(|| {
                panic!("{route}'s turn_stats frame has no `reasoning_tokens` key at all: {stats}")
            });
        assert!(
            reported.is_null(),
            "{route} reported an uncounted turn as a number. `Some(0)` asserts the \
             model thought nothing, which is what PAI-5 P5 will size \
             `output_reserve_tokens` from; `null` says nobody counted. Frame: {stats}"
        );
        // Vacuity control for the assertion above: the frame is a real frame
        // with real numbers in it, so `is_null` is not passing against an empty
        // object or a missing key on a frame that was never emitted.
        assert_eq!(stats["ttft_ms"], 412, "frame: {stats}");
        assert_eq!(stats["completion_tokens"], 88, "frame: {stats}");
    }
}

/// The other half of what thinking cost. `inference_count` cannot derive it: a
/// turn that ran twice for a tool call and one steered back by `EMPTY_TURN_STEER`
/// read the same there. Only `reengagements` separates them, and the second is a
/// whole extra turn paid at full price, prefill and tools included.
#[tokio::test]
async fn both_stream_routes_report_what_the_empty_turn_recovery_cost() {
    for (i, route) in STREAM_ROUTES.iter().enumerate() {
        let h = make_app(Arc::new(StatsAgent {
            reasoning: Some(240),
            reengagements: 2,
        }))
        .await;
        let frames = drive(&h.app, route, &format!("reengaged-{i}")).await;
        let stats = turn_stats_of(&frames, route);

        assert_eq!(
            stats["reengagements"], 2,
            "{route} dropped the re-engagement count. The turn went silent twice \
             and was steered back twice; a client reading this frame would see a \
             slow turn with no reason for it. Frame: {stats}"
        );
        // The count must be its own fact, not a restatement of one already in
        // the frame. `inference_count` is 1 here while `reengagements` is 2 --
        // if a future change ever derives one from the other, this parts them.
        assert_eq!(
            stats["inference_count"], 1,
            "re-engagements leaked into the inference count. Frame: {stats}"
        );
    }
}

/// Zero is a measurement, not an absence, and the ordinary turn is the one that
/// reports it. A frame that omits the field for the common case teaches every
/// client to treat missing as zero, and then the count that matters cannot be
/// distinguished from a client that never learned to read it.
#[tokio::test]
async fn an_ordinary_turn_reports_zero_re_engagements_rather_than_nothing() {
    for (i, route) in STREAM_ROUTES.iter().enumerate() {
        let h = make_app(Arc::new(StatsAgent {
            reasoning: Some(240),
            reengagements: 0,
        }))
        .await;
        let frames = drive(&h.app, route, &format!("ordinary-{i}")).await;
        let stats = turn_stats_of(&frames, route);

        let reported = stats
            .as_object()
            .and_then(|o| o.get("reengagements"))
            .unwrap_or_else(|| {
                panic!("{route}'s turn_stats frame has no `reengagements` key at all: {stats}")
            });
        assert_eq!(
            reported, 0,
            "{route} reported an ordinary turn's re-engagement count as {reported}. \
             Frame: {stats}"
        );
        // Vacuity control: this is a real frame with real numbers, so the
        // assertion above is not passing against an empty object.
        assert_eq!(stats["ttft_ms"], 412, "frame: {stats}");
    }
}

/// A frame after `done` is a frame no client reads: the desktop closes the
/// EventSource on `done` and reloads the session.
#[tokio::test]
async fn the_stats_frame_arrives_before_the_stream_closes_on_both_routes() {
    for (i, route) in STREAM_ROUTES.iter().enumerate() {
        let h = make_app(Arc::new(StatsAgent {
            reasoning: Some(240),
            reengagements: 0,
        }))
        .await;
        let frames = drive(&h.app, route, &format!("ordering-{i}")).await;

        let stats_at = frames
            .iter()
            .position(|f| f["type"] == "turn_stats")
            .unwrap_or_else(|| panic!("{route} emitted no turn_stats frame: {frames:?}"));
        let done_at = frames
            .iter()
            .position(|f| f["done"] == true)
            .unwrap_or_else(|| panic!("{route} never closed its stream: {frames:?}"));
        assert!(
            stats_at < done_at,
            "{route} yielded its turn_stats frame at {stats_at} and its done at \
             {done_at}. A client that stops listening on `done` never sees it."
        );
    }
}

// ── The summary ──────────────────────────────────────────────────────────────

async fn usage_summary(app: &axum::Router) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/usage/summary")
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
    serde_json::from_slice(&bytes).unwrap()
}

/// Persist an assistant row the way `ChatService::persist_assistant_response`
/// does on the `pond chat` and voice paths — the only paths that carry a
/// reasoning count into `session_messages` today.
async fn persist_assistant(
    storage: &Arc<SqliteSessionStorage>,
    session_id: &str,
    id: &str,
    reasoning: Option<u32>,
) {
    let message = SessionMessage::new(
        id.to_string(),
        session_id.to_string(),
        ChatMessage::assistant("a considered answer"),
    )
    .with_token_counts(Some(1000), Some(88))
    .with_reasoning_tokens(reasoning);

    storage
        .add_message(session_id.to_string(), message)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_usage_summary_totals_reasoning_and_says_how_many_turns_counted_one() {
    let h = make_app(Arc::new(StatsAgent {
        reasoning: None,
        reengagements: 0,
    }))
    .await;

    // Two sessions, so the sum is really a sum across sessions and not one
    // session's number reported twice.
    for session in ["sess-a", "sess-b"] {
        h.storage.create_session(session.to_string()).await.unwrap();
        // Real provider totals on the session row, so the `total_tokens`
        // assertion below is checked against non-zero numbers rather than
        // being satisfied by 0 == 0 + 0.
        h.storage
            .increment_usage(session, 1000, 88, Some("test-model"))
            .await
            .unwrap();
    }
    persist_assistant(&h.storage, "sess-a", "m-1", Some(240)).await;
    persist_assistant(&h.storage, "sess-a", "m-2", Some(60)).await;
    persist_assistant(&h.storage, "sess-b", "m-3", Some(100)).await;
    // The row a `/chat/stream` turn writes: counted by nobody, because
    // `persist_assistant_turn` takes a `(prompt, completion)` tuple that cannot
    // carry a third number. It must contribute neither tokens nor a turn.
    persist_assistant(&h.storage, "sess-b", "m-4", None).await;

    let summary = usage_summary(&h.app).await;

    assert_eq!(
        summary["total_reasoning_tokens"], 400,
        "240 + 60 + 100, across two sessions, with the uncounted row \
         contributing nothing. Summary: {summary}"
    );
    assert_eq!(
        summary["counted_reasoning_turns"], 3,
        "a NULL row must not be counted as a turn that thought nothing. \
         Summary: {summary}"
    );
    // The pre-existing figures are untouched: the reasoning estimate is not
    // folded into `total_tokens`, which is what the cloud prices multiply.
    assert_eq!(summary["session_count"], 2, "summary: {summary}");
    assert_eq!(summary["total_prompt_tokens"], 2000, "summary: {summary}");
    assert_eq!(
        summary["total_completion_tokens"], 176,
        "summary: {summary}"
    );
    assert_eq!(
        summary["total_tokens"],
        summary["total_prompt_tokens"].as_u64().unwrap()
            + summary["total_completion_tokens"].as_u64().unwrap(),
        "`total_tokens` must stay prompt + completion. Folding a GIAP estimate \
         into the figure the cloud prices multiply puts an estimate inside a \
         cost. Summary: {summary}"
    );
}

/// The half that makes a zero readable: where nothing has ever counted, both
/// figures are zero TOGETHER. Without `counted_reasoning_turns` that state is
/// indistinguishable from a pond whose models never think, which is the reading
/// PAI-5 P5 must not take from an empty corpus.
#[tokio::test]
async fn a_pond_where_nothing_counted_reports_zero_turns_not_just_zero_tokens() {
    let h = make_app(Arc::new(StatsAgent {
        reasoning: None,
        reengagements: 0,
    }))
    .await;
    h.storage
        .create_session("sess-quiet".to_string())
        .await
        .unwrap();
    persist_assistant(&h.storage, "sess-quiet", "m-1", None).await;

    let summary = usage_summary(&h.app).await;
    assert_eq!(summary["total_reasoning_tokens"], 0, "summary: {summary}");
    assert_eq!(
        summary["counted_reasoning_turns"], 0,
        "zero tokens over zero counted turns is `nobody counted`; zero over a \
         positive count would be `the models did no thinking`. Summary: {summary}"
    );
    // Vacuity control: the summary really did see the session and its row, so
    // the two zeros above are not the answer to an empty database.
    assert_eq!(summary["session_count"], 1, "summary: {summary}");
    assert_eq!(summary["total_completion_tokens"], 0, "summary: {summary}");
}
