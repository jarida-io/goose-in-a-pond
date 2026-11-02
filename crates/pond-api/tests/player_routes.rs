//! Apple Music host routes over the real router: developer-token signing, the MusicKit sign-in
//! page and user-token hand-back, extension egress gating, and host-only secrets staying out of
//! the extension env. Its own binary: network mode and the egress sink are process-global.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use base64::{engine::general_purpose::STANDARD, Engine};
use pond_api::oauth_callback::internal_extension_token;
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort, ToolInfo,
};
use pond_core::mcp::ports::mcp_server::{McpServerConfig, McpServerRepository};
use pond_core::mcp::services::marketplace::BundledMarketplace;
use pond_core::security::domain::event::{Event, EventQuery};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::services::egress::{set_egress_sink, set_network_mode, NetworkMode};
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
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower::ServiceExt;

const EXT: &str = "music";
const HOST_ONLY: [&str; 3] = [
    "APPLE_MUSIC_TEAM_ID",
    "APPLE_MUSIC_KEY_ID",
    "APPLE_MUSIC_PRIVATE_KEY",
];

// ── Fixtures ─────────────────────────────────────────────────────────────────

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

#[derive(Default)]
struct InMemorySecrets {
    values: RwLock<HashMap<String, String>>,
}

#[async_trait::async_trait]
impl SecretRepository for InMemorySecrets {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.values.read().await.get(key).cloned())
    }
    async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.values
            .write()
            .await
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.values.write().await.remove(key);
        Ok(())
    }
    async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.values.read().await.keys().cloned().collect())
    }
    async fn has(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.values.read().await.contains_key(key))
    }
}

struct Installed(Vec<McpServerConfig>);

impl Installed {
    fn none() -> Arc<Self> {
        Arc::new(Self(vec![]))
    }

    fn enabled(name: &str) -> Arc<Self> {
        Arc::new(Self(vec![McpServerConfig {
            id: "test-id".into(),
            name: name.into(),
            kind: "stdio".into(),
            description: String::new(),
            command: Some("npx".into()),
            args: vec![],
            env: HashMap::new(),
            uri: None,
            enabled: true,
            created_at: "2026-09-29T00:00:00Z".into(),
        }]))
    }
}

#[async_trait::async_trait]
impl McpServerRepository for Installed {
    async fn list(&self) -> anyhow::Result<Vec<McpServerConfig>> {
        Ok(self.0.clone())
    }
    async fn save(&self, _: &McpServerConfig) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_enabled(&self, _: &str, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Records every spawn, so a test can read the env the extension would have been given.
#[derive(Default)]
struct CapturingManager {
    added: Mutex<Vec<AddExtensionRequest>>,
}

impl CapturingManager {
    fn last_env(&self) -> HashMap<String, String> {
        self.added
            .lock()
            .unwrap()
            .last()
            .expect("the extension was spawned")
            .env
            .clone()
    }
}

#[async_trait::async_trait]
impl ExtensionManagerPort for CapturingManager {
    async fn list_extensions(&self) -> anyhow::Result<Vec<ExtensionInfo>> {
        Ok(vec![])
    }
    async fn add_extension(&self, request: AddExtensionRequest) -> anyhow::Result<ExtensionInfo> {
        let info = ExtensionInfo {
            name: request.name.clone(),
            kind: request.kind.clone(),
            description: request.description.clone(),
            tools: vec![],
            enabled: true,
            status: "connected".into(),
            last_error: None,
            status_reason: None,
        };
        self.added.lock().unwrap().push(request);
        Ok(info)
    }
    async fn remove_extension(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn list_tools(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn list_tools_detailed(&self) -> anyhow::Result<Vec<ToolInfo>> {
        Ok(vec![])
    }
    async fn set_enabled(&self, _: &str, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Pond {
    app: axum::Router,
    secrets: Arc<InMemorySecrets>,
    _tmp: tempfile::TempDir,
}

async fn pond(manager: Option<Arc<CapturingManager>>, installed: Arc<Installed>) -> Pond {
    // Unset means the built-in address, which is Jarida's real service. A test that wants managed mode
    // sets its own mock through `ManagedAt`; every other one must never leave the machine.
    if std::env::var_os("POND_CREDENTIALS_URL").is_none() {
        std::env::set_var("POND_CREDENTIALS_URL", "off");
    }
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;
    let secrets = Arc::new(InMemorySecrets::default());

    let state = Arc::new(AppState {
        suggestion_queue: Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(db.system.clone()),
        ),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:1/whisper".into(),
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
        room_registry: Arc::new(pond_core::user_data::mocks::mock_room_registry::MockRoomRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        lane: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: Some(tmp.path().to_path_buf()),
        skip_onboarding: true,
        engine_availability: pond_core::models::domain::engine::EngineAvailability {
            litert_library: true,
        },
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        warmup: Default::default(),
        account_sync: None,
        extension_manager: manager.map(|m| m as Arc<dyn ExtensionManagerPort>),
        mcp_server_repo: Some(installed),
        tool_registry: None,
        marketplace: Some(Arc::new(BundledMarketplace::new())),
        secret_repo: Some(secrets.clone()),
        download_tracker: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
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
        mcp_app_resources: HashMap::new(),
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
        setup_claims: None,
        spoken_codes: Default::default(),
        voice_samples: Default::default(),
        pond_admin: None,
    });

    Pond {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        secrets,
        _tmp: tmp,
    }
}

/// A fresh P-256 key per call, so no key material is ever committed.
fn generated_key() -> (Vec<u8>, Vec<u8>) {
    let pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
    let pkcs8 = pair.to_pkcs8v1().unwrap().as_ref().to_vec();
    (pkcs8, pair.public_key().as_ref().to_vec())
}

/// What the modal's single-line input leaves of a pasted `.p8`: the PEM with newlines as spaces.
fn pasted_key(der: &[u8]) -> String {
    format!(
        "-----BEGIN PRIVATE KEY----- {} -----END PRIVATE KEY-----",
        STANDARD.encode(der)
    )
}

impl Pond {
    async fn store_signing_secrets(&self) -> Vec<u8> {
        let (der, public) = generated_key();
        self.secrets
            .set("APPLE_MUSIC_TEAM_ID", "TEAMID1234")
            .await
            .unwrap();
        self.secrets
            .set("APPLE_MUSIC_KEY_ID", "KEYID12345")
            .await
            .unwrap();
        self.secrets
            .set("APPLE_MUSIC_PRIVATE_KEY", &pasted_key(&der))
            .await
            .unwrap();
        public
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, String) {
        let resp = self
            .app
            .clone()
            .oneshot(req)
            .await
            .expect("router responded");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body readable");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, text) = self
            .send(request(method, uri, bearer, body, LOOPBACK))
            .await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

const LOOPBACK: [u8; 4] = [127, 0, 0, 1];
const LAN: [u8; 4] = [192, 168, 1, 50];

fn request(
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
    peer: [u8; 4],
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let mut req = match body {
        Some(b) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((peer, 50_000))));
    req
}

fn verify(token: &str, public: &[u8]) -> Value {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
    validation.required_spec_claims.clear();
    jsonwebtoken::decode::<Value>(
        token,
        &jsonwebtoken::DecodingKey::from_ec_der(public),
        &validation,
    )
    .expect("the token verifies against the stored key's public half")
    .claims
}

// ── Managed credentials ──────────────────────────────────────────────────────

/// `POND_CREDENTIALS_URL` is process-global, so every test that reads or sets it takes a turn.
/// Where a test also needs the network mode, it takes that turn first, so two never wait on each other.
static MANAGED_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Points the pond at a credentials service, and back at none when it goes out of scope.
struct ManagedAt;

impl ManagedAt {
    fn set(url: &str) -> ManagedAt {
        std::env::set_var("POND_CREDENTIALS_URL", url);
        ManagedAt
    }
}

impl Drop for ManagedAt {
    fn drop(&mut self) {
        // Back to `off`, not to unset: unset now means Jarida's real service, which no test may call.
        std::env::set_var("POND_CREDENTIALS_URL", "off");
    }
}

/// A credentials service on loopback, counting how often it is asked.
struct CredentialsService {
    url: String,
    hits: Arc<std::sync::atomic::AtomicUsize>,
}

async fn credentials_service(status: StatusCode, body: Value) -> CredentialsService {
    use axum::extract::State as AxumState;
    use std::sync::atomic::Ordering;

    #[derive(Clone)]
    struct Shared {
        hits: Arc<std::sync::atomic::AtomicUsize>,
        reply: (StatusCode, Value),
    }
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new()
        .route(
            "/v1/musickit/developer-token",
            axum::routing::post(|AxumState(s): AxumState<Shared>| async move {
                s.hits.fetch_add(1, Ordering::SeqCst);
                (s.reply.0, axum::Json(s.reply.1))
            }),
        )
        .with_state(Shared {
            hits: hits.clone(),
            reply: (status, body),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    CredentialsService {
        url: format!("http://127.0.0.1:{port}"),
        hits,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn a_token(days: u64) -> Value {
    json!({ "token": "aaaa.bbbb.cccc", "expires_at": unix_now() + days * 86_400 })
}

const DEV_TOKEN: &str = "/api/v1/musickit/developer-token";

#[tokio::test]
async fn with_no_key_stored_the_shared_token_is_used_and_kept() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    let _managed = MANAGED_TURN.lock().await;
    egress_log();
    set_network_mode(NetworkMode::Open);
    let svc = credentials_service(StatusCode::OK, a_token(30)).await;
    let _at = ManagedAt::set(&svc.url);
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["token"], "aaaa.bbbb.cccc");

    let (_, again) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
    assert_eq!(again["token"], "aaaa.bbbb.cccc");
    assert_eq!(
        svc.hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a month-long token is fetched once, not on every ask"
    );

    let event = recorded("egress.http", "127.0.0.1", "giap-credentials").await;
    assert_eq!(event.attributes.get("method"), Some(&"POST".into()));
}

#[tokio::test]
async fn a_key_the_household_stored_beats_the_shared_one() {
    let _managed = MANAGED_TURN.lock().await;
    let svc = credentials_service(StatusCode::OK, a_token(30)).await;
    let _at = ManagedAt::set(&svc.url);
    let pond = pond(None, Installed::none()).await;
    let public = pond.store_signing_secrets().await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["token"], "aaaa.bbbb.cccc");
    assert_eq!(
        verify(body["token"].as_str().unwrap(), &public)["iss"],
        "TEAMID1234"
    );
    assert_eq!(
        svc.hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no network for their own key"
    );
}

// ── A kept token survives a restart ──────────────────────────────────────────

const KEPT: &str = "APPLE_MUSIC_MANAGED_TOKEN";

/// What an earlier start would have stored for `url`, with `days` of life left and fetched `ago` ago.
fn kept(url: &str, days: u64, fetched_days_ago: u64) -> String {
    let t = a_token(days);
    json!({
        "url": url,
        "token": t["token"],
        "expires_at": t["expires_at"],
        "fetched_at": unix_now().saturating_sub(fetched_days_ago * 86_400),
    })
    .to_string()
}

#[tokio::test]
async fn a_token_fetched_once_is_kept_and_a_restart_does_not_ask_again() {
    let _managed = MANAGED_TURN.lock().await;
    let svc = credentials_service(StatusCode::OK, a_token(30)).await;
    let _at = ManagedAt::set(&svc.url);
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        pond.secrets.has(KEPT).await.unwrap(),
        "the token is kept for the next start"
    );

    // A restart forgets memory and nothing else.
    pond_api::musickit::forget_managed_memory().await;
    let (status, again) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["token"], "aaaa.bbbb.cccc");
    assert_eq!(
        svc.hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the second start used the kept token and never reached the service"
    );
}

#[tokio::test]
async fn a_kept_token_that_is_due_is_renewed_and_still_served_if_the_renewal_fails() {
    let _managed = MANAGED_TURN.lock().await;
    // Down for the renewal: the service answers 503.
    let down = credentials_service(StatusCode::SERVICE_UNAVAILABLE, json!({})).await;
    let _at = ManagedAt::set(&down.url);
    let pond = pond(None, Installed::none()).await;
    // 4 days left of 30 is under a fifth, so it is due; it has not expired.
    pond.secrets
        .set(KEPT, &kept(&down.url, 4, 26))
        .await
        .unwrap();
    pond_api::musickit::forget_managed_memory().await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a token with days left is not thrown away: {body}"
    );
    assert_eq!(body["token"], "aaaa.bbbb.cccc");
    assert_eq!(
        down.hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "it did try to renew"
    );
}

#[tokio::test]
async fn a_kept_token_that_is_due_is_replaced_when_the_service_is_up() {
    let _managed = MANAGED_TURN.lock().await;
    let fresh = json!({ "token": "dddd.eeee.ffff", "expires_at": unix_now() + 30 * 86_400 });
    let svc = credentials_service(StatusCode::OK, fresh).await;
    let _at = ManagedAt::set(&svc.url);
    let pond = pond(None, Installed::none()).await;
    pond.secrets
        .set(KEPT, &kept(&svc.url, 4, 26))
        .await
        .unwrap();
    pond_api::musickit::forget_managed_memory().await;

    let (_, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;

    assert_eq!(
        body["token"], "dddd.eeee.ffff",
        "the due token was replaced"
    );
    let stored = pond.secrets.get(KEPT).await.unwrap().unwrap();
    assert!(
        stored.contains("dddd.eeee.ffff"),
        "and the new one is what is kept"
    );
}

#[tokio::test]
async fn a_kept_token_is_not_served_for_another_address_or_after_it_lapses_or_when_damaged() {
    let _managed = MANAGED_TURN.lock().await;
    let svc = credentials_service(StatusCode::OK, a_token(30)).await;
    let _at = ManagedAt::set(&svc.url);

    for (label, raw) in [
        ("another address", kept("https://other.example.org", 20, 10)),
        (
            "a lapsed expiry",
            json!({"url": svc.url, "token": "aaaa.bbbb.cccc", "expires_at": unix_now() - 1_000, "fetched_at": unix_now() - 40 * 86_400})
                .to_string(),
        ),
        ("damaged text", "not json at all".to_string()),
        (
            "a token that is not one",
            json!({"url": svc.url, "token": "<script>", "expires_at": unix_now() + 86_400 * 20, "fetched_at": unix_now()})
                .to_string(),
        ),
    ] {
        let before = svc.hits.load(std::sync::atomic::Ordering::SeqCst);
        let pond = pond(None, Installed::none()).await;
        pond.secrets.set(KEPT, &raw).await.unwrap();
        pond_api::musickit::forget_managed_memory().await;

        let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;

        assert_eq!(status, StatusCode::OK, "{label}: {body}");
        assert_eq!(
            svc.hits.load(std::sync::atomic::Ordering::SeqCst),
            before + 1,
            "{label} is ignored, and the pond asks the service"
        );
    }
}

// ── The probe: can a token be had, without having one ────────────────────────

#[tokio::test]
async fn the_probe_says_a_token_is_possible_and_never_reaches_the_service() {
    let _managed = MANAGED_TURN.lock().await;
    let svc = credentials_service(StatusCode::OK, a_token(30)).await;
    let _at = ManagedAt::set(&svc.url);
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond
        .call(
            "GET",
            "/api/v1/musickit/developer-token?probe=true",
            Some("test-token"),
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["available"], true);
    assert!(
        body.get("token").is_none(),
        "a probe hands out no token: {body}"
    );
    assert_eq!(
        svc.hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a probe is not a call to Jarida"
    );
    assert!(!pond.secrets.has(KEPT).await.unwrap(), "and keeps nothing");
}

#[tokio::test]
async fn the_probe_says_what_is_missing_when_nothing_could_supply_a_token() {
    let _managed = MANAGED_TURN.lock().await;
    let _at = ManagedAt::set("off");
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond
        .call(
            "GET",
            "/api/v1/musickit/developer-token?probe=true",
            Some("test-token"),
            None,
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("Team ID") && error.contains("Developer settings"),
        "{error}"
    );
}

#[tokio::test]
async fn the_probe_counts_a_stored_key_and_needs_no_network_for_it() {
    let _managed = MANAGED_TURN.lock().await;
    let _at = ManagedAt::set("off");
    let pond = pond(None, Installed::none()).await;
    pond.store_signing_secrets().await;

    let (status, body) = pond
        .call(
            "GET",
            "/api/v1/musickit/developer-token?probe=true",
            Some("test-token"),
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["available"], true);
    assert!(body.get("token").is_none(), "{body}");
}

#[tokio::test]
async fn the_probe_is_for_the_paired_page_like_the_token_itself() {
    let _managed = MANAGED_TURN.lock().await;
    let _at = ManagedAt::set("off");
    let pond = pond(None, Installed::none()).await;
    let uri = "/api/v1/musickit/developer-token?probe=true";

    assert_eq!(
        pond.call("GET", uri, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        pond.call("GET", uri, Some(internal_extension_token()), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "an extension may not probe either"
    );
}

#[tokio::test]
async fn a_service_that_fails_says_why_and_is_not_asked_again_at_once() {
    let _managed = MANAGED_TURN.lock().await;
    let svc = credentials_service(StatusCode::TOO_MANY_REQUESTS, json!({})).await;
    let _at = ManagedAt::set(&svc.url);
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body["error"].as_str().unwrap().contains("busy"), "{body}");

    // The player retries every ten seconds; a service that is down must not be hammered by it.
    let (again, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
    assert_eq!(again, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("busy"));
    assert_eq!(svc.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_reply_that_is_not_a_token_is_refused() {
    let _managed = MANAGED_TURN.lock().await;
    for reply in [
        json!({ "token": "<script>alert(1)</script>", "expires_at": unix_now() + 86_400 * 30 }),
        json!({ "token": "aaaa.bbbb.cccc", "expires_at": unix_now() + 86_400 * 900 }),
        json!({ "token": "aaaa.bbbb.cccc" }),
        json!({}),
    ] {
        let svc = credentials_service(StatusCode::OK, reply.clone()).await;
        let _at = ManagedAt::set(&svc.url);
        let pond = pond(None, Installed::none()).await;
        let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{reply}: {body}");
    }
}

#[tokio::test]
async fn a_pond_set_to_offline_sends_nothing_and_says_which_setting() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    let _managed = MANAGED_TURN.lock().await;
    egress_log();
    set_network_mode(NetworkMode::Offline);
    let _at = ManagedAt::set("https://credentials.example.invalid");
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("network_mode") && error.contains("credentials.example.invalid"),
        "{error}"
    );
    recorded(
        "egress.denied",
        "credentials.example.invalid",
        "giap-credentials",
    )
    .await;

    set_network_mode(NetworkMode::Open);
}

#[tokio::test]
async fn an_address_that_is_not_https_is_ignored_so_the_pond_falls_back_to_saying_what_to_add() {
    let _managed = MANAGED_TURN.lock().await;
    let _at = ManagedAt::set("http://credentials.example.org");
    let pond = pond(None, Installed::none()).await;

    let (status, body) = pond.call("GET", DEV_TOKEN, Some("test-token"), None).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("Team ID"));
}

async fn apple_configured(pond: &Pond) -> Value {
    pond.call(
        "GET",
        "/api/v1/player/status",
        Some(internal_extension_token()),
        None,
    )
    .await
    .1["apple"]["configured"]
        .clone()
}

#[tokio::test]
async fn the_service_counts_as_a_token_source_for_the_status_route() {
    let _managed = MANAGED_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    assert_eq!(
        apple_configured(&pond).await,
        false,
        "no key and no service"
    );

    let _at = ManagedAt::set("https://credentials.example.org");
    assert_eq!(
        apple_configured(&pond).await,
        true,
        "the extension may now choose the in-app player"
    );
}

// ── Developer token ──────────────────────────────────────────────────────────

#[tokio::test]
async fn developer_token_is_for_the_paired_page_and_never_for_an_extension() {
    let _managed = MANAGED_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    pond.store_signing_secrets().await;
    let uri = "/api/v1/musickit/developer-token";

    let (status, body) = pond.call("GET", uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _) = pond
        .call("GET", uri, Some(internal_extension_token()), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an extension holds the internal token, which is not a session: it never gets a signing token"
    );

    let (status, body) = pond.call("GET", uri, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn developer_token_without_the_signing_secrets_says_what_to_add() {
    let _managed = MANAGED_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    let uri = "/api/v1/musickit/developer-token";
    let (status, body) = pond.call("GET", uri, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = body["error"].as_str().unwrap();
    assert!(
        error.contains("Team ID") && error.contains("private key"),
        "{error}"
    );

    pond.secrets
        .set("APPLE_MUSIC_TEAM_ID", "TEAMID1234")
        .await
        .unwrap();
    pond.secrets
        .set("APPLE_MUSIC_KEY_ID", "KEYID12345")
        .await
        .unwrap();
    pond.secrets
        .set("APPLE_MUSIC_PRIVATE_KEY", "this is not a key")
        .await
        .unwrap();
    let (status, body) = pond.call("GET", uri, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains(".p8"), "{body}");
}

#[tokio::test]
async fn developer_token_is_signed_with_the_stored_key_for_a_week() {
    let _managed = MANAGED_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    let public = pond.store_signing_secrets().await;
    let (status, body) = pond
        .call(
            "GET",
            "/api/v1/musickit/developer-token",
            Some("test-token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let token = body["token"].as_str().unwrap();
    let header = jsonwebtoken::decode_header(token).unwrap();
    assert_eq!(header.kid.as_deref(), Some("KEYID12345"));
    let claims = verify(token, &public);
    assert_eq!(claims["iss"], "TEAMID1234");
    assert_eq!(claims["exp"], body["expires_at"]);
    assert_eq!(
        claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
        7 * 24 * 60 * 60
    );
}

// ── Host-only secrets never reach the extension env ──────────────────────────

fn assert_env_withholds_signing_secrets(env: &HashMap<String, String>) {
    for key in HOST_ONLY {
        assert!(
            !env.contains_key(key),
            "{key} reached the extension env: {:?}",
            env.keys()
        );
    }
}

#[tokio::test]
async fn install_stores_and_reports_host_only_secrets_but_withholds_them_from_env() {
    let manager = Arc::new(CapturingManager::default());
    let pond = pond(Some(manager.clone()), Installed::none()).await;
    let (der, _) = generated_key();
    let (status, body) = pond
        .call(
            "POST",
            &format!("/api/v1/marketplace/{EXT}/install"),
            Some("test-token"),
            Some(json!({ "secrets": {
                "APPLE_MUSIC_TEAM_ID": "TEAMID1234",
                "APPLE_MUSIC_KEY_ID": "KEYID12345",
                "APPLE_MUSIC_PRIVATE_KEY": pasted_key(&der),
                "MUSIC_SERVICE": "apple",
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let env = manager.last_env();
    assert_env_withholds_signing_secrets(&env);
    assert_eq!(env.get("MUSIC_SERVICE").map(String::as_str), Some("apple"));

    for key in HOST_ONLY {
        assert!(pond.secrets.has(key).await.unwrap(), "{key} was not stored");
    }
    let (_, secrets) = pond
        .call(
            "GET",
            &format!("/api/v1/extensions/{EXT}/secrets"),
            Some("test-token"),
            None,
        )
        .await;
    for key in HOST_ONLY {
        assert_eq!(
            secrets["fulfilled"][key], true,
            "{key} must read as saved: {secrets}"
        );
    }
}

#[tokio::test]
async fn install_with_no_secrets_at_all_succeeds() {
    let manager = Arc::new(CapturingManager::default());
    let pond = pond(Some(manager.clone()), Installed::none()).await;
    let (status, body) = pond
        .call(
            "POST",
            &format!("/api/v1/marketplace/{EXT}/install"),
            Some("test-token"),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "every music secret is optional: {body}"
    );
}

#[tokio::test]
async fn a_secrets_change_restarts_the_extension_without_the_host_only_ones() {
    let manager = Arc::new(CapturingManager::default());
    let pond = pond(Some(manager.clone()), Installed::enabled(EXT)).await;
    let (der, _) = generated_key();
    let (status, body) = pond
        .call(
            "POST",
            &format!("/api/v1/extensions/{EXT}/secrets"),
            Some("test-token"),
            Some(json!({
                "APPLE_MUSIC_TEAM_ID": "TEAMID1234",
                "APPLE_MUSIC_KEY_ID": "KEYID12345",
                "APPLE_MUSIC_PRIVATE_KEY": pasted_key(&der),
                "MUSIC_SERVICE": "apple",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restarted"], true, "{body}");

    let env = manager.last_env();
    assert_env_withholds_signing_secrets(&env);
    // A restart passes what the registry declares for the extension: the music choice reaches it,
    // since the extension follows it; Spotify's token and client ID are host-only and never do.
    assert_eq!(env.get("MUSIC_SERVICE").map(String::as_str), Some("apple"));
    assert!(
        !env.contains_key("SPOTIFY_ACCESS_TOKEN"),
        "Spotify's token reached the extension"
    );
    assert!(
        !env.contains_key("SPOTIFY_CLIENT_ID"),
        "Spotify's client ID reached the extension"
    );
    for key in HOST_ONLY {
        assert!(pond.secrets.has(key).await.unwrap(), "{key} was not stored");
    }
}

// ── Extension egress ─────────────────────────────────────────────────────────

/// `set_network_mode` is process-global, so egress tests take turns.
static NETWORK_MODE_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct CapturingLog(Arc<Mutex<Vec<Event>>>);

#[async_trait::async_trait]
impl EventLog for CapturingLog {
    async fn append(&self, event: Event) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
    async fn query(&self, _: EventQuery) -> anyhow::Result<Vec<Event>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn purge(&self, _: EventQuery) -> anyhow::Result<u64> {
        Ok(0)
    }
}

fn egress_log() -> &'static Arc<Mutex<Vec<Event>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<Event>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        set_egress_sink(Arc::new(CapturingLog(log.clone())));
        log
    })
}

/// The sink appends on a spawned task, so wait for the event rather than racing it.
async fn recorded(action: &str, host: &str, tool: &str) -> Event {
    for _ in 0..200 {
        let found = egress_log()
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| {
                e.action == action
                    && e.attributes.get("host") == Some(&host.into())
                    && e.attributes.get("tool") == Some(&tool.into())
            })
            .cloned();
        if let Some(event) = found {
            return event;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no {action} event for {host} via {tool}");
}

async fn ask_egress(pond: &Pond, url: &str) -> (StatusCode, Value) {
    pond.call(
        "POST",
        "/api/v1/extension/egress",
        Some(internal_extension_token()),
        Some(json!({ "url": url, "method": "GET", "extension": EXT })),
    )
    .await
}

#[tokio::test]
async fn egress_is_for_extensions_only_and_rejects_malformed_asks() {
    let pond = pond(None, Installed::none()).await;
    let (status, _) = pond
        .call(
            "POST",
            "/api/v1/extension/egress",
            Some("test-token"),
            Some(json!({ "url": "https://itunes.apple.com/", "extension": EXT })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for body in [
        json!({ "extension": EXT }),
        json!({ "url": "https://itunes.apple.com/" }),
        json!({ "url": "not a url", "extension": EXT }),
        json!({ "url": "https://itunes.apple.com/", "extension": "bad name" }),
    ] {
        let (status, _) = pond
            .call(
                "POST",
                "/api/v1/extension/egress",
                Some(internal_extension_token()),
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn offline_refuses_apple_and_records_the_refusal_against_the_extension() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    egress_log();
    set_network_mode(NetworkMode::Offline);
    let pond = pond(None, Installed::none()).await;

    let (status, body) = ask_egress(&pond, "https://itunes.apple.com/search?term=x").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allowed"], false);
    let reason = body["reason"].as_str().unwrap();
    assert!(
        reason.contains("network_mode") && reason.contains("itunes.apple.com"),
        "{reason}"
    );
    let event = recorded("egress.denied", "itunes.apple.com", "giap-music").await;
    assert_eq!(
        event.attributes.get("network_mode"),
        Some(&"offline".into())
    );

    let (_, body) = ask_egress(&pond, "http://127.0.0.1:4000/api/v1/health").await;
    assert_eq!(body["allowed"], true, "loopback stays allowed offline");
    recorded("egress.http", "127.0.0.1", "giap-music").await;
}

#[tokio::test]
async fn open_allows_apple_and_records_it_via_the_extension() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    egress_log();
    set_network_mode(NetworkMode::Open);
    let pond = pond(None, Installed::none()).await;

    let (status, body) = ask_egress(&pond, "https://itunes.apple.com/lookup?id=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "allowed": true }));
    let event = recorded("egress.http", "itunes.apple.com", "giap-music").await;
    assert_eq!(event.attributes.get("method"), Some(&"GET".into()));
}

// ── The exemptions are exact ─────────────────────────────────────────────────

#[tokio::test]
async fn an_unrelated_route_still_needs_a_token_from_the_host() {
    let pond = pond(None, Installed::none()).await;
    let (status, _) = pond.call("GET", "/api/v1/secrets", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── The player bridge ────────────────────────────────────────────────────────

/// One page's command stream, read a server-sent event at a time.
struct PageStream {
    body: axum::body::BodyDataStream,
    buf: String,
}

impl PageStream {
    async fn next(&mut self) -> (String, Value) {
        use futures::StreamExt;
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let frame = self.buf[..end].to_string();
                self.buf = self.buf[end + 2..].to_string();
                if frame.starts_with(':') {
                    continue;
                }
                let mut event = String::new();
                let mut data = String::new();
                for line in frame.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim());
                    }
                }
                return (event, serde_json::from_str(&data).unwrap_or(Value::Null));
            }
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(3), self.body.next())
                .await
                .expect("an event within 3s")
                .expect("the stream is open")
                .expect("a readable chunk");
            self.buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
}

/// The page opens its stream, as the player window does, and is told it is attached.
async fn attach(pond: &Pond, service: &str) -> PageStream {
    let resp = pond
        .app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/player/events?service={service}"),
            Some("test-token"),
            None,
            LOOPBACK,
        ))
        .await
        .expect("router responded");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut page = PageStream {
        body: resp.into_body().into_data_stream(),
        buf: String::new(),
    };
    let (event, data) = page.next().await;
    assert_eq!(event, "ready");
    assert_eq!(data["service"], service);
    page
}

/// An extension's command, sent on its own task so the test can play the page meanwhile.
fn send_command(
    pond: &Pond,
    service: &str,
    op: &str,
    args: Value,
    timeout_ms: u64,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = pond.app.clone();
    let body = json!({ "service": service, "op": op, "args": args, "timeout_ms": timeout_ms });
    tokio::spawn(async move {
        let resp = app
            .oneshot(request(
                "POST",
                "/api/v1/player/command",
                Some(internal_extension_token()),
                Some(body),
                LOOPBACK,
            ))
            .await
            .expect("router responded");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    })
}

async fn page_replies(pond: &Pond, id: &str, ok: bool, result: Value) -> Value {
    pond.call(
        "POST",
        "/api/v1/player/reply",
        Some("test-token"),
        Some(json!({ "id": id, "ok": ok, "result": result, "error": if ok { Value::Null } else { json!("no") }, "code": if ok { Value::Null } else { json!("player_error") } })),
    )
    .await
    .1
}

async fn read_state(pond: &Pond, service: &str) -> (StatusCode, Value) {
    pond.call(
        "GET",
        &format!("/api/v1/player/state?service={service}"),
        Some("test-token"),
        None,
    )
    .await
}

async fn player_status(pond: &Pond) -> Value {
    pond.call(
        "GET",
        "/api/v1/player/status",
        Some(internal_extension_token()),
        None,
    )
    .await
    .1
}

/// The player page asks with its own session, like its other routes.
async fn ask_policy(pond: &Pond, url: &str) -> (StatusCode, Value) {
    pond.call(
        "POST",
        "/api/v1/player/egress-policy",
        Some("test-token"),
        Some(json!({ "url": url, "method": "GET" })),
    )
    .await
}

#[tokio::test]
async fn a_command_travels_to_the_page_and_its_reply_returns() {
    let pond = pond(None, Installed::none()).await;
    let mut page = attach(&pond, "t-roundtrip").await;

    let call = send_command(&pond, "t-roundtrip", "play", json!({ "id": "1001" }), 5_000);
    let (event, command) = page.next().await;
    assert_eq!(event, "command");
    assert_eq!(command["op"], "play");
    assert_eq!(command["args"]["id"], "1001");
    assert_eq!(command["service"], "t-roundtrip");

    let accepted = page_replies(
        &pond,
        command["id"].as_str().unwrap(),
        true,
        json!({ "playing": true }),
    )
    .await;
    assert_eq!(accepted["accepted"], true);

    let (status, body) = call.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": true, "result": { "playing": true } }));
}

#[tokio::test]
async fn a_failure_the_page_reports_comes_back_with_its_code() {
    let pond = pond(None, Installed::none()).await;
    let mut page = attach(&pond, "t-failure").await;

    let call = send_command(&pond, "t-failure", "play", json!({}), 5_000);
    let (_, command) = page.next().await;
    page_replies(&pond, command["id"].as_str().unwrap(), false, Value::Null).await;

    let (status, body) = call.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    assert_eq!(body["code"], "player_error");
}

#[tokio::test]
async fn no_page_is_reported_as_no_player() {
    let pond = pond(None, Installed::none()).await;
    let (status, body) = send_command(&pond, "t-nobody", "pause", json!({}), 1_000)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    assert_eq!(body["code"], "no_player");
    // The page is in the browser now, so the words say where to open it.
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("The music player is not open"), "{error}");
    assert!(error.contains("web browser"), "{error}");
}

#[tokio::test]
async fn a_page_that_does_not_answer_times_out() {
    let pond = pond(None, Installed::none()).await;
    let _page = attach(&pond, "t-silent").await;
    let (_, body) = send_command(&pond, "t-silent", "pause", json!({}), 200)
        .await
        .unwrap();
    assert_eq!(body["code"], "timeout");
}

#[tokio::test]
async fn reopening_the_player_answers_the_call_it_orphaned() {
    let pond = pond(None, Installed::none()).await;
    let mut first = attach(&pond, "t-replace").await;

    let call = send_command(&pond, "t-replace", "play", json!({}), 5_000);
    let (event, _) = first.next().await;
    assert_eq!(event, "command", "the first page has the command in hand");
    let _second = attach(&pond, "t-replace").await;

    let (_, body) = call.await.unwrap();
    assert_eq!(body["code"], "player_replaced");
}

#[tokio::test]
async fn commands_and_status_are_for_extensions_only() {
    let pond = pond(None, Installed::none()).await;
    for (method, uri, body) in [
        (
            "POST",
            "/api/v1/player/command",
            Some(json!({ "service": "apple", "op": "pause" })),
        ),
        ("GET", "/api/v1/player/status", None),
    ] {
        let (status, _) = pond.call(method, uri, None, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} with no token");

        let (status, _) = pond
            .call(method, uri, Some("test-token"), body.clone())
            .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri}: a paired device is not an extension"
        );

        let (status, _) = pond
            .send(request(
                method,
                uri,
                Some(internal_extension_token()),
                body,
                LAN,
            ))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} from the LAN");
    }
}

#[tokio::test]
async fn the_page_needs_a_session_and_a_service_name() {
    let pond = pond(None, Installed::none()).await;
    for (method, uri) in [
        ("GET", "/api/v1/player/events?service=apple"),
        ("POST", "/api/v1/player/reply"),
        ("POST", "/api/v1/player/state"),
        ("GET", "/api/v1/player/state?service=apple"),
    ] {
        let (status, _) = pond.call(method, uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }

    for uri in [
        "/api/v1/player/events",
        "/api/v1/player/events?service=Bad%20Name",
        "/api/v1/player/state?service=",
    ] {
        let (status, _) = pond.call("GET", uri, Some("test-token"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
    }

    let (status, _) = pond
        .call(
            "POST",
            "/api/v1/player/command",
            Some(internal_extension_token()),
            Some(json!({ "service": "apple", "op": "Not An Op" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn what_the_page_reports_is_readable_and_forgotten_when_it_closes() {
    let pond = pond(None, Installed::none()).await;
    let now_playing = json!({ "playing": true, "title": "Nairobi" });

    let (status, _) = pond
        .call(
            "POST",
            "/api/v1/player/state",
            Some("test-token"),
            Some(json!({ "service": "t-state", "state": now_playing })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "state from a page that never attached"
    );

    let page = attach(&pond, "t-state").await;
    let (status, _) = pond
        .call(
            "POST",
            "/api/v1/player/state",
            Some("test-token"),
            Some(json!({ "service": "t-state", "state": now_playing })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = read_state(&pond, "t-state").await;
    assert_eq!(body["attached"], true);
    assert_eq!(body["state"], now_playing);

    drop(page);
    for _ in 0..100 {
        if read_state(&pond, "t-state").await.1["attached"] == false {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (_, body) = read_state(&pond, "t-state").await;
    assert_eq!(body["attached"], false, "a closed page is detached");
    assert_eq!(
        body["state"],
        Value::Null,
        "and has no now-playing left behind"
    );
}

#[tokio::test]
async fn status_says_which_services_have_a_page_and_which_have_credentials() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    let _managed = MANAGED_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    let before = player_status(&pond).await;
    assert_eq!(before["apple"]["configured"], false, "{before}");
    assert_eq!(before["apple"]["attached"], false);

    pond.store_signing_secrets().await;
    let _page = attach(&pond, "apple").await;
    let after = player_status(&pond).await;
    assert_eq!(after["apple"]["configured"], true, "{after}");
    assert_eq!(after["apple"]["attached"], true);
}

// ── Spotify: the page's user token ───────────────────────────────────────────

const USER_TOKEN: &str = "/api/v1/player/user-token?service=spotify";

#[tokio::test]
async fn the_user_token_is_for_the_paired_page_and_never_for_an_extension() {
    let pond = pond(None, Installed::none()).await;
    pond.secrets
        .set("SPOTIFY_ACCESS_TOKEN", "BQD-stored-token")
        .await
        .unwrap();

    let (status, body) = pond.call("GET", USER_TOKEN, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _) = pond
        .call("GET", USER_TOKEN, Some(internal_extension_token()), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an extension holds the internal token, which is not a session"
    );

    let (status, body) = pond.call("GET", USER_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["token"], "BQD-stored-token");
}

#[tokio::test]
async fn the_user_token_without_a_sign_in_says_where_to_sign_in() {
    let pond = pond(None, Installed::none()).await;
    let (status, body) = pond.call("GET", USER_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("sign in to Spotify"),
        "{body}"
    );

    // An empty stored value is not a sign-in either.
    pond.secrets.set("SPOTIFY_ACCESS_TOKEN", "").await.unwrap();
    let (status, _) = pond.call("GET", USER_TOKEN, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_user_token_is_for_spotify_and_nothing_else() {
    let pond = pond(None, Installed::none()).await;
    pond.secrets
        .set("SPOTIFY_ACCESS_TOKEN", "BQD-stored-token")
        .await
        .unwrap();
    for service in ["apple", "tidal", "", "spotify%2Fx"] {
        let uri = format!("/api/v1/player/user-token?service={service}");
        let (status, body) = pond.call("GET", &uri, Some("test-token"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{service}: {body}");
        assert!(body.get("token").is_none(), "{service}: {body}");
    }
    let (status, _) = pond
        .call("GET", "/api/v1/player/user-token", Some("test-token"), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no service named");
}

#[tokio::test]
async fn a_renewal_that_cannot_happen_says_so_and_returns_no_stale_token() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    pond.secrets
        .set("SPOTIFY_ACCESS_TOKEN", "BQD-stale")
        .await
        .unwrap();
    // No refresh token is stored, so there is nothing to renew with, and the answer must not be
    // the token the SDK just told us stopped working.
    let uri = format!("{USER_TOKEN}&refresh=true");
    let (status, body) = pond.call("GET", &uri, Some("test-token"), None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.get("token").is_none(), "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("Sign in to Spotify again"),
        "{body}"
    );
}

#[tokio::test]
async fn status_reports_whether_spotify_is_signed_in() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    let _managed = MANAGED_TURN.lock().await;
    let pond = pond(None, Installed::none()).await;
    let before = player_status(&pond).await;
    assert_eq!(before["spotify"]["configured"], false, "{before}");
    assert_eq!(before["spotify"]["attached"], false, "{before}");

    pond.secrets
        .set("SPOTIFY_ACCESS_TOKEN", "BQD-stored-token")
        .await
        .unwrap();
    let _page = attach(&pond, "spotify").await;
    let after = player_status(&pond).await;
    assert_eq!(after["spotify"]["configured"], true, "{after}");
    assert_eq!(after["spotify"]["attached"], true, "{after}");
}

#[tokio::test]
async fn offline_refuses_what_reaches_the_service_but_never_the_pause_button() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    egress_log();
    set_network_mode(NetworkMode::Offline);
    let pond = pond(None, Installed::none()).await;
    let mut page = attach(&pond, "apple").await;

    let (status, body) = send_command(&pond, "apple", "search", json!({ "q": "x" }), 1_000)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    assert_eq!(body["code"], "refused");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("api.music.apple.com"),
        "{body}"
    );
    recorded("egress.denied", "api.music.apple.com", "giap-music-apple").await;

    let call = send_command(&pond, "apple", "pause", json!({}), 5_000);
    let (event, command) = page.next().await;
    assert_eq!(
        event, "command",
        "the refused search never reached the page; pause did"
    );
    assert_eq!(command["op"], "pause");
    page_replies(&pond, command["id"].as_str().unwrap(), true, Value::Null).await;
    assert_eq!(call.await.unwrap().1["ok"], true);

    set_network_mode(NetworkMode::Open);
}

#[tokio::test]
async fn the_shell_asks_the_network_policy_about_the_players_requests() {
    let _turn = NETWORK_MODE_TURN.lock().await;
    egress_log();
    let pond = pond(None, Installed::none()).await;
    set_network_mode(NetworkMode::Offline);
    let (status, body) = ask_policy(
        &pond,
        "https://js-cdn.music.apple.com/musickit/v3/musickit.js",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allowed"], false, "{body}");
    recorded("egress.denied", "js-cdn.music.apple.com", "giap-player").await;
    let (_, body) = ask_policy(&pond, "http://127.0.0.1:4000/api/v1/health").await;
    assert_eq!(body["allowed"], true, "loopback stays allowed offline");

    set_network_mode(NetworkMode::Open);
    let (_, body) = ask_policy(
        &pond,
        "https://js-cdn.music.apple.com/musickit/v3/musickit.js",
    )
    .await;
    assert_eq!(body["allowed"], true);
    recorded("egress.http", "js-cdn.music.apple.com", "giap-player").await;

    let (status, _) = pond
        .send(request(
            "POST",
            "/api/v1/player/egress-policy",
            None,
            Some(json!({ "url": "https://js-cdn.music.apple.com/" })),
            LAN,
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "nothing off the host may ask"
    );
    let (status, _) = pond
        .call(
            "POST",
            "/api/v1/player/egress-policy",
            None,
            Some(json!({ "url": "https://js-cdn.music.apple.com/" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "being on the host is not a session"
    );
}
