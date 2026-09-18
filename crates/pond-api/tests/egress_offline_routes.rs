//! PAI-2 P6b: `routes.rs` holds sixteen `.send()` sites while `egress_guard.rs` only demands
//! one gated call per file, so this file is the other half: every send is paired with its own
//! gate in source, and under `Offline` the routes must REFUSE rather than merely fail. NOTE:
//! `set_network_mode` is process-global: every test here installs `Offline`, none may use `Open`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::services::egress::{set_network_mode, NetworkMode};
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use tower::ServiceExt;

// ── The source guard ─────────────────────────────────────────────────────────

const ROUTES_RS: &str = "src/routes.rs";

/// Any of these, called in code, opens a gated hop. Call forms with the opening paren, and
/// the source has its comments stripped first, or the guard certifies comment prose that
/// merely mentions `egress::begin`. The entries MUST NOT overlap: a superstring entry counts
/// one real call twice, which left every test here green with a real gate deleted.
const GATE_CALLS: &[&str] = &["egress::begin(", "egress::check_egress("];

/// The sends that are deliberately ungated, each with the loopback literal that is the
/// reason. Named individually and pinned by count: a heuristic exemption does not merely
/// miss a new hole, it LOCKS IT OUT of the question.
struct UngatedSend {
    /// Prefix of the enclosing top-level `fn` line.
    function: &'static str,
    /// Why it is not egress -- a literal that must still be in that function.
    loopback_literal: &'static str,
}

const UNGATED_LOOPBACK_SENDS: &[UngatedSend] = &[
    UngatedSend {
        function: "tts_synthesise(",
        loopback_literal: "http://127.0.0.1:{}/tts",
    },
    UngatedSend {
        function: "sync_ollama_models(",
        loopback_literal: "http://localhost:11434/api/tags",
    },
    UngatedSend {
        function: "list_ollama_models(",
        loopback_literal: "http://localhost:11434/api/tags",
    },
];

/// Total `.send()` sites in `routes.rs` production source. A vacuity control:
/// if the detector breaks, the pairing loop finds nothing and reports success.
const EXPECTED_SENDS: usize = 16;

/// Remove every `#[cfg(test)]` ITEM. Line-based for the same reason `egress_guard.rs` is: a
/// `format!("{{")` inside a test desynchronises a brace counter, and rustfmt guarantees an
/// item's closing brace sits at its own indentation. Truncating at the FIRST `#[cfg(test)]`
/// would be wrong -- this file has several, with production code between them.
fn production_source(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.trim_start().starts_with("#[cfg(test)]") {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let opens_block = lines
            .get(i + 1)
            .map(|l| l.trim_end().ends_with('{'))
            .unwrap_or(false);
        if !opens_block {
            i += 2;
            continue;
        }
        let closer = format!("{}}}", " ".repeat(indent));
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim_end() != closer {
            j += 1;
        }
        i = j + 1;
    }
    out
}

/// Strip `//` line comments, string-literal aware so a `"https://…"` does not
/// eat the rest of its line.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut cut = line.len();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// `routes.rs` split into one chunk per top-level `fn` / `async fn`.
fn routes_fn_chunks() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ROUTES_RS);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let code = strip_line_comments(&production_source(&src));
    code.split("\nfn ")
        .flat_map(|c| c.split("\nasync fn "))
        .map(|c| c.to_string())
        .collect()
}

/// Byte offsets of every occurrence of `needle` in `hay`.
fn offsets_of(hay: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = hay[from..].find(needle) {
        let at = from + i;
        out.push(at);
        from = at + needle.len();
    }
    out
}

/// Every `.send()` in `routes.rs` is preceded by a gate of its OWN. "This function contains a
/// gate somewhere" is the weaker claim, and it stays true when a second request is
/// copy-pasted under an existing gated one. So the pairing is sequential: walking a function,
/// the Nth send must be preceded by at least N gates.
#[test]
fn every_send_in_routes_rs_pairs_with_its_own_gate() {
    let chunks = routes_fn_chunks();

    let mut total_sends = 0usize;
    let mut exempt_hits = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for chunk in &chunks {
        let sends = offsets_of(chunk, ".send()");
        if sends.is_empty() {
            continue;
        }
        total_sends += sends.len();

        let name = chunk.lines().next().unwrap_or("<unknown>").to_string();

        if let Some(exempt) = UNGATED_LOOPBACK_SENDS
            .iter()
            .find(|e| name.starts_with(e.function))
        {
            exempt_hits += 1;
            // The exemption has to keep being true. Same polarity as
            // `egress_guard.rs :: loopback_exemptions_contain_no_third_party_url`:
            // "it only talks to Ollama" must not be a claim in a comment, and
            // comments are stripped above, so this reads the real literal.
            assert!(
                chunk.contains(exempt.loopback_literal),
                "`{}` is exempt from the egress gate because it only talks to \
                 `{}`, and that literal is no longer in the function. Either it \
                 moved (update the entry) or the destination changed -- in which \
                 case gate the call instead of keeping the exemption.",
                exempt.function,
                exempt.loopback_literal
            );
            continue;
        }

        let mut gates: Vec<usize> = Vec::new();
        for call in GATE_CALLS {
            gates.extend(offsets_of(chunk, call));
        }
        gates.sort_unstable();
        gates.dedup();

        for (nth, send_at) in sends.iter().enumerate() {
            let available = gates.iter().filter(|g| **g < *send_at).count();
            if available <= nth {
                failures.push(format!(
                    "`{}`: send #{} (byte {send_at}) has only {available} gate(s) \
                     before it, and {} earlier send(s) already consumed them. \
                     Every outbound request needs its OWN check -- a second \
                     `.send()` under someone else's gate is an ungated hop.",
                    name.trim_end(),
                    nth + 1,
                    nth
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "ungated sends in {ROUTES_RS}:\n  {}\n\
         Gate it with `pond_core::shared::services::egress::begin(url, method)` \
         (or `check_egress` where there is nothing to time), or -- if it is \
         genuinely loopback-only -- add it to UNGATED_LOOPBACK_SENDS with the \
         literal that proves it and raise the pinned count deliberately.",
        failures.join("\n  ")
    );

    // Vacuity controls. Both numbers are pinned, and both only move by hand.
    assert_eq!(
        exempt_hits,
        UNGATED_LOOPBACK_SENDS.len(),
        "expected {} exempt loopback senders, matched {exempt_hits}. A named \
         exemption whose function was renamed or deleted stops exempting \
         anything and starts hiding the fact that the list is stale.",
        UNGATED_LOOPBACK_SENDS.len()
    );
    assert_eq!(
        total_sends, EXPECTED_SENDS,
        "found {total_sends} `.send()` sites in {ROUTES_RS}, expected \
         {EXPECTED_SENDS}. If the count dropped to 0 the detector has broken, \
         not the code. If a send was added, this test already checked it has a \
         gate -- update the number. If one was removed, update it too."
    );
}

// ── The behavioural tests ────────────────────────────────────────────────────

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

/// A secret store that already holds a Spotify token pair, so `spotify_api_call` gets past
/// its "not connected" early returns and reaches the gate. Without a stored token the routes
/// return "not connected" before touching the network, and a test asserting "no request left
/// the pond" would pass against a handler that has no gate at all.
struct ConnectedSpotify;

#[async_trait::async_trait]
impl SecretRepository for ConnectedSpotify {
    async fn get(&self, _key: &str) -> anyhow::Result<Option<String>> {
        Ok(Some("stored-token".to_string()))
    }
    async fn set(&self, _key: &str, _value: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn has(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
}

/// `whisper_url` is deliberately a REMOTE host here. It is a free-text setting
/// (`voice_whisper_url`) that merely defaults to loopback, so a pond pointed at
/// a remote ASR box ships raw household audio to a third party -- and both the
/// `/transcribe` forward and the `/test` diagnostics probe used it ungated.
const REMOTE_WHISPER: &str = "https://asr.example.com";

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        suggestion_queue: std::sync::Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: REMOTE_WHISPER.into(),
        transcribe_audio: None,
        session_storage,
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
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
        data_dir: Some(tmp.path().to_path_buf()),
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: Some(Arc::new(ConnectedSpotify)),
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

/// Install `Offline` and build a router. Every test starts here.
async fn offline_app() -> (axum::Router, tempfile::TempDir) {
    set_network_mode(NetworkMode::Offline);
    make_app().await
}

async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.oneshot(req).await.expect("router responded");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body readable");
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, json: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("Authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(json.to_string()))
        .unwrap()
}

/// A refusal, not a failure. Both leave the caller with no data; only one of
/// them tells the user which setting produced that.
fn assert_is_refusal(text: &str, host: &str) {
    assert!(
        text.contains("network_mode"),
        "expected an egress REFUSAL naming the setting, got: {text:?}. A \
         connection error and a refusal are not the same finding -- if this is \
         a DNS or TCP message, the request was actually made."
    );
    assert!(
        text.contains(host),
        "the refusal must name the destination it refused ({host}), got: {text:?}"
    );
    assert!(
        text.contains("offline"),
        "the refusal must name the mode that produced it, got: {text:?}"
    );
}

#[tokio::test]
async fn hugging_face_model_search_is_refused_offline() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(app, get("/api/v1/models/search/gguf?q=gemma")).await;
    // Status first: a body predicate on an error payload reports whatever the
    // absence of a key means, which is the opposite of the truth.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["models"], serde_json::json!([]));
    assert_is_refusal(body["error"].as_str().unwrap_or_default(), "huggingface.co");
}

#[tokio::test]
async fn hugging_face_repo_file_listing_is_refused_offline() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(app, get("/api/v1/models/search/gguf/files?repo=a/b")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["files"], serde_json::json!([]));
    assert_is_refusal(body["error"].as_str().unwrap_or_default(), "huggingface.co");
}

#[tokio::test]
async fn github_llamafile_release_listing_is_refused_offline() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(app, get("/api/v1/models/search/llamafile")).await;
    assert_eq!(status, StatusCode::OK);
    assert_is_refusal(body["error"].as_str().unwrap_or_default(), "api.github.com");
}

/// The one route that takes an arbitrary caller-supplied URL, so it is the one
/// place in the file where "which host" is not decided by GIAP.
#[tokio::test]
async fn a_model_download_by_url_is_refused_offline_before_it_spawns() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(
        app,
        post(
            "/api/v1/models/download/url",
            serde_json::json!({
                "url": "https://huggingface.co/org/repo/resolve/main/model.gguf",
                "category": "gguf",
                "filename": "model.gguf",
            }),
        ),
    )
    .await;
    // 502, not 202. A refusal that still returns "downloading" and fails inside
    // a detached task is not an answer to "why is nothing downloading".
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_is_refusal(body["error"].as_str().unwrap_or_default(), "huggingface.co");
}

#[tokio::test]
async fn spotify_now_playing_reports_a_refusal_not_a_disconnection() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(app, get("/api/v1/music/now-playing")).await;
    assert_eq!(status, StatusCode::OK);
    // The distinction is the point. Before P6b a refusal came back as a bare
    // `{"connected": false}`, which sends the user to re-run an OAuth flow that
    // cannot possibly succeed while the mode is what it is.
    assert_eq!(body["error"], "network_refused");
    assert_is_refusal(
        body["message"].as_str().unwrap_or_default(),
        "api.spotify.com",
    );
}

#[tokio::test]
async fn spotify_control_is_refused_offline() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(
        app,
        post(
            "/api/v1/music/control",
            serde_json::json!({"action": "pause"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "network_refused");
    assert_is_refusal(
        body["error"].as_str().unwrap_or_default(),
        "api.spotify.com",
    );
}

/// The diagnostics probe, and with it the whole `voice_whisper_url` question.
/// `/api/v1/test` probes three URLs: the two 127.0.0.1 literals must stay unaffected under
/// `Offline`, since a privacy control that reports the local model server as blocked gets
/// switched off, and the third, the setting, is the one that has to be refused.
#[tokio::test]
async fn the_diagnostics_probe_refuses_a_remote_whisper_and_leaves_loopback_alone() {
    let (app, _tmp) = offline_app().await;
    let (status, body) = send(app, get("/api/v1/test")).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        body["whisper"]["status"], "refused",
        "a remote voice_whisper_url must be refused under offline, got: {}",
        body["whisper"]
    );
    assert_is_refusal(
        body["whisper"]["error"].as_str().unwrap_or_default(),
        "asr.example.com",
    );

    // The vacuity control for the whole file: loopback still passes the gate.
    // Without this, an `egress_verdict` that refused everything would make every
    // assertion above pass while breaking the pond.
    for local in ["llamafile", "ollama"] {
        assert_ne!(
            body[local]["status"], "refused",
            "{local} is probed at a 127.0.0.1 literal and must never be refused \
             by the network mode; got: {}",
            body[local]
        );
    }
}
