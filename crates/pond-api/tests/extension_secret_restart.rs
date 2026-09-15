//! A credential change that cannot reach the extension must say so.
//!
//! `POST /extensions/{name}/secrets` used to store the values and return
//! `{"stored": n}`, restarting nothing — so the desktop reported "Credentials
//! updated" over an extension still running with the old ones. The OAuth
//! callback did restart, but only inside an `if let (Some(mgr), Some(mp),
//! Some(repo))` with no `else`, so on a backend without an extension manager
//! it skipped silently and rendered a green "Connected" page over a dead
//! extension.
//!
//! Both paths now go through `restart_extension_with_secrets`, which reports
//! why it could not act. These tests pin the two halves of that contract: a
//! restart that cannot happen is surfaced, and an extension the user has not
//! installed is left alone.
//!
//! Run: cargo test -p pond-api --test extension_secret_restart

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::mcp_server::{McpServerConfig, McpServerRepository};
use pond_core::mcp::services::marketplace::BundledMarketplace;
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::mocks::mock_agent::MockAgent;
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
use tokio::sync::RwLock;
use tower::ServiceExt;

/// The marketplace id of a bundled stdio extension that requires secrets.
const EXT: &str = "music";

const REMOTE_WHISPER: &str = "http://127.0.0.1:1/whisper";

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

/// Accepts and remembers whatever is stored, so the handler's own storage step
/// succeeds and the assertions are only ever about the restart.
#[derive(Default)]
struct InMemorySecrets {
    values: RwLock<std::collections::HashMap<String, String>>,
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

/// Reports a fixed set of installed extensions — the handler reads this to
/// decide whether the extension is one the user actually runs.
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
            env: std::collections::HashMap::new(),
            uri: None,
            enabled: true,
            created_at: "2026-09-04T00:00:00Z".into(),
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

async fn make_app(installed: Arc<Installed>) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
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
        warmup: Default::default(),
        account_sync: None,
        extension_manager: None, // the point of the test: no manager to start anything
        mcp_server_repo: Some(installed.clone()),
        tool_registry: None,
        marketplace: Some(Arc::new(BundledMarketplace::new())),
        secret_repo: Some(Arc::new(InMemorySecrets::default())),
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

async fn post_secrets(
    installed: Arc<Installed>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (app, _tmp) = make_app(installed).await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/extensions/{EXT}/secrets"))
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.expect("router responded");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body readable");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_restart_that_cannot_happen_is_reported() {
    let (status, body) = post_secrets(
        Installed::enabled(EXT),
        serde_json::json!({ "SPOTIFY_CLIENT_ID": "abc" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "the secret was stored: {body}");
    assert_eq!(body["stored"], 1);
    assert_eq!(
        body["restarted"], false,
        "there is no extension manager, so nothing was restarted: {body}"
    );
    assert!(
        body["restart_error"].is_string(),
        "the caller must be told the credentials are not in use yet, got: {body}"
    );
}

#[tokio::test]
async fn an_extension_the_user_has_not_installed_is_left_alone() {
    let (status, body) = post_secrets(
        Installed::none(),
        serde_json::json!({ "SPOTIFY_CLIENT_ID": "abc" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "the secret was stored: {body}");
    assert_eq!(body["stored"], 1);
    assert_eq!(body["restarted"], false);
    assert!(
        body["restart_error"].is_null(),
        "nothing to restart is not a failure, got: {body}"
    );
}
