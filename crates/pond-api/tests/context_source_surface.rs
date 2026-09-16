//! PAI-8 P1: a household member can connect a sensor or camera as personal
//! context, and nobody else can. The phase's security decision is that the owner
//! is RESOLVED, never supplied: no `profile_id` in the body, because every item
//! inherits it and migration 0044 refuses to let it change afterwards.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_profile::SqliteProfileRepository;
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

struct Harness {
    app: axum::Router,
    storage: Arc<SqliteSessionStorage>,
    profiles: Arc<SqliteProfileRepository>,
    secrets: Option<Arc<MemorySecrets>>,
    _tmp: tempfile::TempDir,
}

/// A secret store that only remembers, so a test can assert what was written
/// WITHOUT reaching into the encrypted file adapter. What matters here is that
/// the route stores the password somewhere the source can find it again, and
/// that it never comes back out of the API.
#[derive(Default)]
struct MemorySecrets {
    inner: tokio::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[async_trait::async_trait]
impl pond_core::security::ports::secret::SecretRepository for MemorySecrets {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.inner.lock().await.get(key).cloned())
    }
    async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.inner
            .lock()
            .await
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.inner.lock().await.remove(key);
        Ok(())
    }
    async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.inner.lock().await.keys().cloned().collect())
    }
    async fn has(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.inner.lock().await.contains_key(key))
    }
}

async fn make_app() -> Harness {
    make_app_inner(false).await
}

/// The same pond, with somewhere to keep an account password.
async fn make_app_with_secrets() -> Harness {
    make_app_inner(true).await
}

async fn make_app_inner(with_secrets: bool) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let storage = Arc::new(SqliteSessionStorage::new(pool.clone()));
    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));
    let secrets = with_secrets.then(|| Arc::new(MemorySecrets::default()));
    let hs = MockHandshake::new();
    hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage: storage.clone(),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: profiles.clone(),
        device_registry: Arc::new(NoDevices),
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
        secret_repo: secrets.clone().map(|s| {
            s as Arc<dyn pond_core::security::ports::secret::SecretRepository + Send + Sync>
        }),
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
        profiles,
        secrets,
        _tmp: tmp,
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

async fn member(h: &Harness, name: &str) -> String {
    h.profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id
}

/// A session that nobody has identified. Resolves to `Household` on a
/// one-member pond and `Guest` once there are two — the posture PAI-1 P3 chose,
/// reached the way production reaches it.
async fn unidentified_session(h: &Harness, id: &str) -> String {
    h.storage.create_session(id.to_string()).await.unwrap();
    id.to_string()
}

/// A session bound to a member, at `Explicit` strength — what
/// `PUT /sessions/{id}/user` writes.
async fn session_of(h: &Harness, id: &str, profile_id: &str) -> String {
    h.storage.create_session(id.to_string()).await.unwrap();
    h.storage
        .set_session_identity(
            id,
            &SessionIdentity {
                profile_id: Some(profile_id.to_string()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();
    id.to_string()
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

// ── Invariant 4: one member's proposals, and nobody else's ───────────────────

async fn post_json(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("Authorization", "Bearer test-token")
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn delete_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn connect_body(kind: &str, provider: &str, session_id: &str) -> Value {
    serde_json::json!({ "kind": kind, "provider": provider, "session_id": session_id })
}

// ── The owner is resolved, never supplied ──────────────────────────────────

#[tokio::test]
async fn a_member_connects_a_camera_and_it_is_theirs() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let session = session_of(&h, "chat-liz", &liz).await;

    let (status, body) = post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["profile_id"], liz);
    assert_eq!(body["id"], "camera:front-door");
}

/// The security claim, and it is structural rather than checked: there is no
/// `profile_id` field on the request type, so this body cannot name an owner.
/// `deny_unknown_fields` is not what stops it -- serde ignores unknown keys
/// here -- so what this asserts is that the extra key changes NOTHING.
#[tokio::test]
async fn a_body_that_names_an_owner_does_not_get_one() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let ada = member(&h, "Ada").await;
    let session = session_of(&h, "chat-liz", &liz).await;

    let (status, body) = post_json(
        &h.app,
        "/api/v1/context/sources",
        serde_json::json!({
            "kind": "camera",
            "provider": "front-door",
            "session_id": session,
            "profile_id": ada,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["profile_id"], liz,
        "the request named Ada as the owner and the pond believed it. Every item this source \
         ever produces would be filed under her, and 0044 refuses to let the owner change \
         afterwards"
    );
}

// ── Guest and Household connect nothing ────────────────────────────────────

#[tokio::test]
async fn a_guest_cannot_connect_a_source_and_sees_none() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let _ada = member(&h, "Ada").await; // two members: unidentified is Guest
    let session = unidentified_session(&h, "chat-anon").await;

    let (status, _) = post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &session),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an unidentified speaker connected a context source"
    );

    // Vacuity control: the same request from a member DOES work, so the refusal
    // above is the scope and not a broken fixture.
    let owned = session_of(&h, "chat-liz", &liz).await;
    let (ok, _) = post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &owned),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);

    // And the guest sees nothing that exists.
    let (status, body) = get_json(
        &h.app,
        &format!("/api/v1/context/sources?session_id={session}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["sources"].as_array().map(|a| a.len()),
        Some(0),
        "a guest was shown a household member's sources"
    );
}

#[tokio::test]
async fn the_whole_household_is_not_an_owner() {
    let h = make_app().await;
    // TWO members, so `Household` genuinely names more than one person.
    let _liz = member(&h, "Liz").await;
    let _jerry = member(&h, "Jerry").await;
    let session = unidentified_session(&h, "chat-anon").await;

    let (status, body) = post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &session),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "Household connected a source. It is not a weaker address than Owner, it is the \
         broadcast, and a source it owned would file one member's camera under everybody: {body}"
    );
}

/// The same scope, and the opposite answer, because the household is one person:
/// `Household` and `Owner(the-only-member)` denote the same set of people, so
/// there is nothing to refuse. This is a sole-member rule, not a Household rule;
/// the multi-member refusal above is the part that survives.
#[tokio::test]
async fn a_one_member_household_is_that_member() {
    let h = make_app().await;
    let jerry = member(&h, "Jerry").await;
    let session = unidentified_session(&h, "chat-anon").await;

    let (status, body) = post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["profile_id"].as_str(),
        Some(jerry.as_str()),
        "the one member should own it: {body}"
    );
}

// No "a guest is refused in a one-member household" test exists because that
// state is unreachable: `identity_resolution::resolve` answers `Guest` only above
// one member and `Household` otherwise. The residual exposure is bounded by the
// caller still needing an authenticated, paired device.

// ── A connector that does not exist is refused up front ────────────────────

#[tokio::test]
async fn a_source_kind_with_no_connector_is_refused_with_the_reason() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let session = session_of(&h, "chat-liz", &liz).await;

    for kind in ["mail", "calendar", "files", "chat", "mobile"] {
        let (status, body) = post_json(
            &h.app,
            "/api/v1/context/sources",
            connect_body(kind, "whatever", &session),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{kind} was accepted. It would sit in the table looking connected and never produce \
             anything"
        );
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("does not exist")
                || body["error"].as_str().unwrap_or("").len() > 20,
            "{kind} was refused without saying what is missing: {body}"
        );
    }

    // Vacuity control: the two kinds that DO have a producer are accepted.
    for kind in ["sensor", "camera"] {
        let (status, _) = post_json(
            &h.app,
            "/api/v1/context/sources",
            connect_body(kind, "hallway", &session),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kind} should be connectable today");
    }
}

// ── Disconnecting says how many items went ─────────────────────────────────

#[tokio::test]
async fn disconnecting_reports_the_number_of_items_removed() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let session = session_of(&h, "chat-liz", &liz).await;

    post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &session),
    )
    .await;

    let (status, body) = delete_json(
        &h.app,
        &format!("/api/v1/context/sources/camera:front-door?session_id={session}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.get("items_removed").is_some(),
        "invariant 6 is 'deletes its items by default AND SAYS HOW MANY'. The count is \
         disconnect_source's return value precisely so a caller cannot satisfy the invariant \
         without reporting it: {body}"
    );

    let (_, after) = get_json(
        &h.app,
        &format!("/api/v1/context/sources?session_id={session}"),
    )
    .await;
    assert_eq!(after["sources"].as_array().map(|a| a.len()), Some(0));
}

#[tokio::test]
async fn one_member_never_sees_another_members_sources() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let ada = member(&h, "Ada").await;
    let liz_session = session_of(&h, "chat-liz", &liz).await;
    let ada_session = session_of(&h, "chat-ada", &ada).await;

    post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("camera", "front-door", &liz_session),
    )
    .await;

    let (_, mine) = get_json(
        &h.app,
        &format!("/api/v1/context/sources?session_id={liz_session}"),
    )
    .await;
    assert_eq!(mine["sources"].as_array().map(|a| a.len()), Some(1));

    let (_, theirs) = get_json(
        &h.app,
        &format!("/api/v1/context/sources?session_id={ada_session}"),
    )
    .await;
    assert_eq!(
        theirs["sources"].as_array().map(|a| a.len()),
        Some(0),
        "Ada was shown Liz's camera"
    );
}

// ── PAI-8 P4: connecting an account ──────────────────────────────────────────

/// A calendar source with no credentials would be a row that looks connected
/// and can never sync -- the empty-source shape `availability` exists to
/// prevent, arriving through the front door instead of around it.
#[tokio::test]
async fn a_calendar_without_a_password_is_refused() {
    let h = make_app_with_secrets().await;
    let jerry = member(&h, "Jerry").await;
    let session = session_of(&h, "s-cal-1", &jerry).await;

    let (status, body) = post_json(
        &h.app,
        "/api/v1/context/sources",
        connect_body("calendar", "fastmail", &session),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("app password"),
        "the refusal should say what is needed: {body}"
    );
}

/// The mirror. A sensor is already on the pond, so sign-in details for one are
/// a sign the caller has confused two things, and storing them would put a
/// credential in the store that nothing will ever read or delete.
#[tokio::test]
async fn a_sensor_with_a_password_is_refused_too() {
    let h = make_app_with_secrets().await;
    let jerry = member(&h, "Jerry").await;
    let session = session_of(&h, "s-cal-2", &jerry).await;

    let mut body = connect_body("sensor", "hall-pir", &session);
    body["credentials"] = serde_json::json!({"username": "a", "password": "b"});
    let (status, resp) = post_json(&h.app, "/api/v1/context/sources", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {resp}");
}

#[tokio::test]
async fn a_connected_calendar_keeps_its_password_in_the_secret_store_and_not_in_the_reply() {
    let h = make_app_with_secrets().await;
    let jerry = member(&h, "Jerry").await;
    let session = session_of(&h, "s-cal-3", &jerry).await;

    let mut body = connect_body("calendar", "fastmail", &session);
    body["credentials"] =
        serde_json::json!({"username": "jerry@example.org", "password": "app-secret-xyz"});
    let (status, resp) = post_json(&h.app, "/api/v1/context/sources", body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");

    // The owner is in the id, so two members can each connect their own account
    // instead of the second colliding with the first.
    let id = resp["id"].as_str().expect("id");
    assert!(
        id.contains(&jerry),
        "the source id should carry its owner: {id}"
    );

    // The password is in the store...
    let secrets = h.secrets.clone().expect("secret store");
    use pond_core::security::ports::secret::SecretRepository as _;
    let stored = secrets
        .get(&pond_core::context::domain::secret_key_for(id))
        .await
        .unwrap()
        .expect("the credentials were not stored where the sync will look for them");
    assert!(stored.contains("app-secret-xyz"));

    // ...and nowhere in what the API said back.
    let rendered = resp.to_string();
    assert!(
        !rendered.contains("app-secret-xyz"),
        "the reply echoed the password: {rendered}"
    );
}

/// A pond with no encrypted store must refuse rather than drop the password
/// (a source that can never sync) or put it somewhere unencrypted (invariant 4).
#[tokio::test]
async fn a_pond_with_no_secret_store_refuses_to_hold_a_password() {
    let h = make_app().await;
    let jerry = member(&h, "Jerry").await;
    let session = session_of(&h, "s-cal-4", &jerry).await;

    let mut body = connect_body("calendar", "fastmail", &session);
    body["credentials"] = serde_json::json!({"username": "a", "password": "b"});
    let (status, resp) = post_json(&h.app, "/api/v1/context/sources", body).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {resp}");
}
