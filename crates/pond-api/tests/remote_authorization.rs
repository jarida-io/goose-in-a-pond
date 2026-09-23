//! Remote authorization boundaries against real SQLite-issued sessions.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::Notification;
use pond_core::shared::mocks::mock_agent::MockAgent;

use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

use pond_infra::db::Database;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use serde_json::Value;
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    remote: axum::Router,
    handshake: Arc<SqliteHandshakeAdapter>,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));

    let handshake = Arc::new(SqliteHandshakeAdapter::new(pool.clone(), None));
    let state = Arc::new(AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(pond_infra::onboarding::SqlxOnboardingRepository::new(
            pool.clone(),
        )),
        // Real issuance and validation keep caller attribution tied to the
        // stored session, rather than teaching a mock the desired identity.
        handshake: handshake.clone(),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: profiles.clone(),
        device_registry: Arc::new(SqliteDeviceRegistry::new(pool.clone())),
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
        data_dir: Some(tmp.path().to_path_buf()),
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
        push_token_repo: Some(Arc::new(
            pond_infra::sqlite_push_token::SqlitePushTokenRepository::new(pool.clone()),
        )),
        notification_tx: tokio::sync::broadcast::channel::<Notification>(16).0,
        notification_queue: Some(Arc::new(
            pond_infra::sqlite_notification_queue::SqliteNotificationQueue::new(pool.clone()),
        )),
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

    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone()).layer(axum::Extension(ConnectInfo(
        SocketAddr::from(([127, 0, 0, 1], 40_000)),
    )));
    let remote = build_router(state, dist).layer(axum::Extension(ConnectInfo(SocketAddr::from((
        [100, 64, 0, 44],
        40_000,
    )))));

    Harness {
        loopback,
        remote,
        handshake,
        _tmp: tmp,
    }
}

use pond_core::security::ports::handshake::{
    Handshake, HandshakeResponse, InitRequest, VerifyRequest,
};
use serde_json::json;

async fn pair(h: &Harness, id: &str) -> HandshakeResponse {
    let code = h.handshake.issue_pairing_code().await.unwrap();
    use base64::Engine;
    use hmac::Mac;
    let init = h
        .handshake
        .init_handshake(InitRequest {
            client_id: id.into(),
            client_type: "gotg".into(),
            client_version: "test".into(),
        })
        .await
        .unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(code.code.as_bytes()).unwrap();
    mac.update(
        &base64::engine::general_purpose::STANDARD
            .decode(init.challenge)
            .unwrap(),
    );
    mac.update(id.as_bytes());
    let mac: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let result = h
        .handshake
        .verify_handshake(VerifyRequest {
            channel_binding: None,
            challenge_id: init.challenge_id,
            mac,
            device_name: Some(id.into()),
        })
        .await
        .unwrap();
    assert!(result.accepted);
    result
}

async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> axum::response::Response {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json")
        .header("X-Forwarded-For", "127.0.0.1");
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn revoke_requires_bearer_and_cannot_target_another_session() {
    let h = make_app().await;
    let a = pair(&h, "a").await;
    let b = pair(&h, "b").await;
    let a_token = a.session_token.as_deref().unwrap();
    let b_token = b.session_token.as_deref().unwrap();
    let resp = send(
        &h.remote,
        Method::POST,
        "/api/v1/handshake/revoke",
        None,
        json!({"token": b_token}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(h.handshake.validate_token(b_token).await.unwrap());
    let resp = send(
        &h.remote,
        Method::POST,
        "/api/v1/handshake/revoke",
        Some(a_token),
        json!({"token": b_token}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!h.handshake.validate_token(a_token).await.unwrap());
    assert!(h.handshake.validate_token(b_token).await.unwrap());
    let resp = send(
        &h.remote,
        Method::POST,
        "/api/v1/handshake/refresh",
        None,
        json!({"refresh_token": a.refresh_token.unwrap()}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["accepted"], false);
    assert_eq!(body["rejection_reason"], "invalid_or_expired_refresh");
}

#[tokio::test]
async fn notification_and_push_claims_must_match_the_paired_device() {
    let h = make_app().await;
    let a = pair(&h, "a").await;
    let b = pair(&h, "b").await;
    let token = a.session_token.as_deref();
    // Register B's own push token before an attempted overwrite/deletion by A.
    let response = send(
        &h.remote,
        Method::POST,
        "/api/v1/devices/b/push-token",
        b.session_token.as_deref(),
        json!({"platform":"fcm", "token":"original"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    for (method, path, body) in [
        (
            Method::GET,
            "/api/v1/notifications/stream?device_id=b",
            json!(null),
        ),
        (
            Method::GET,
            "/api/v1/notifications/stream?device_id=ghost",
            json!(null),
        ),
        (
            Method::POST,
            "/api/v1/devices/b/push-token",
            json!({"platform":"fcm", "token":"replacement"}),
        ),
        (Method::DELETE, "/api/v1/devices/b/push-token", json!(null)),
    ] {
        let response = send(&h.remote, method, path, token, body).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let db = Database::init(h._tmp.path()).await.unwrap();
    let row: (String,) = sqlx::query_as("SELECT token FROM push_tokens WHERE device_id = 'b'")
        .fetch_one(&db.system)
        .await
        .unwrap();
    assert_eq!(row.0, "original");
}

#[tokio::test]
async fn device_session_roams_and_refreshes_without_ip_binding() {
    let h = make_app().await;
    let a = pair(&h, "a").await;
    for router in [&h.loopback, &h.remote] {
        let response = send(
            router,
            Method::GET,
            "/api/v1/notifications/stream?device_id=a",
            a.session_token.as_deref(),
            json!(null),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
    }
    let response = send(
        &h.remote,
        Method::POST,
        "/api/v1/handshake/refresh",
        None,
        json!({"refresh_token": a.refresh_token.unwrap()}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let refreshed: Value = serde_json::from_slice(&bytes).unwrap();
    let response = send(
        &h.remote,
        Method::GET,
        "/api/v1/notifications/stream?device_id=a",
        refreshed["session_token"].as_str(),
        json!(null),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn expensive_diagnostics_require_auth_but_local_voice_still_works() {
    let h = make_app().await;
    for (method, path) in [
        (Method::POST, "/api/v1/transcribe"),
        (Method::GET, "/api/v1/dev/goose"),
        (Method::POST, "/api/v1/tts"),
        (Method::GET, "/api/v1/test"),
        (Method::POST, "/api/v1/test/speak"),
        (Method::GET, "/"),
    ] {
        let response = send(&h.remote, method, path, None, json!(null)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    for path in ["/api/v1/health", "/api/v1/system/info"] {
        assert_eq!(
            send(&h.remote, Method::GET, path, None, json!(null))
                .await
                .status(),
            StatusCode::OK
        );
    }
    assert_ne!(
        send(
            &h.loopback,
            Method::POST,
            "/api/v1/tts",
            None,
            json!({"text":"hello"})
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn revocation_waits_for_durable_queue_and_uses_authenticated_device() {
    use pond_core::security::ports::remote_access::RemoteRevocation;
    struct Queue {
        failed: std::sync::atomic::AtomicBool,
        device: std::sync::Mutex<Option<String>>,
    }
    #[async_trait::async_trait]
    impl RemoteRevocation for Queue {
        async fn queue(&self, id: &str) -> anyhow::Result<()> {
            if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("storage unavailable");
            }
            *self.device.lock().unwrap() = Some(id.to_owned());
            Ok(())
        }
    }
    let h = make_app().await;
    let a = pair(&h, "phone000000000001").await;
    let token = a.session_token.as_deref().unwrap();
    let queue = Arc::new(Queue {
        failed: true.into(),
        device: Default::default(),
    });
    let router = h
        .remote
        .clone()
        .layer(axum::Extension(queue.clone() as Arc<dyn RemoteRevocation>));
    let response = send(
        &router,
        Method::POST,
        "/api/v1/handshake/revoke",
        Some(token),
        json!({"device_id":"another-device"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(h.handshake.validate_token(token).await.unwrap());
    queue
        .failed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let response = send(
        &router,
        Method::POST,
        "/api/v1/handshake/revoke",
        Some(token),
        json!({"device_id":"another-device"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!h.handshake.validate_token(token).await.unwrap());
    assert_eq!(
        queue.device.lock().unwrap().as_deref(),
        Some("phone000000000001")
    );
}
