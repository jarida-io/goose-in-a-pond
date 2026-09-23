//! PAI-1 P9's HTTP half: a pairing code is bound to a household member at ISSUANCE. The code
//! must reach `pairing_codes.profile_id`; a non-loopback peer is refused even with a member
//! named; an unknown member mints nothing (migration 0043's foreign key); an unparsable body
//! is refused, not defaulted. The code to `devices.profile_id` step lives in sqlite_handshake.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::Notification;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_infra::db::Database;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use serde_json::Value;
use tower::ServiceExt;

struct Harness {
    /// Requests arrive from 127.0.0.1 — the operator's own machine.
    loopback: axum::Router,
    /// The same state, reached from a LAN address.
    remote: axum::Router,
    profiles: Arc<SqliteProfileRepository>,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(pond_infra::onboarding::SqlxOnboardingRepository::new(
            pool.clone(),
        )),
        // The real adapter: the whole point is that the route reaches
        // `issue_pairing_code_for`, and a mock would answer whatever it was
        // told to.
        handshake: Arc::new(SqliteHandshakeAdapter::new(pool.clone(), None)),
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
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel::<Notification>(16).0,
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

    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let remote = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 44],
        40_000,
    ))));

    Harness {
        loopback,
        remote,
        profiles,
        _tmp: tmp,
    }
}

/// `POST /handshake/pairing-code` with a raw body. `body: None` sends no body
/// and no content type at all, which is what the CLI and the desktop dashboard
/// have done since #93 and must keep working.
async fn issue(router: &axum::Router, body: Option<&str>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/handshake/pairing-code");
    let req = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// The live pairing code as `GET /handshake/pairing-code` reports it. A genuine read-back,
/// not an echo: `current_pairing_code` runs a fresh `SELECT`, so `profile_id` is the value
/// the row holds and the one `verify_handshake` later copies onto the device. `code: null`
/// means nothing was minted.
async fn live_code(router: &axum::Router) -> Value {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/handshake/pairing-code")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "the read-back route itself");
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn a_member(h: &Harness, name: &str) -> String {
    h.profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id
}

// ── The capture ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_code_issued_for_a_member_carries_that_member_into_the_row() {
    let h = make_app().await;
    let liz = a_member(&h, "Liz").await;

    let (status, body) = issue(&h.loopback, Some(&format!(r#"{{"profile_id":"{liz}"}}"#))).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["profile_id"], liz,
        "the response must name the member the code was bound to; body: {body}"
    );
    assert!(
        body["code"].as_str().is_some_and(|c| c.len() == 6),
        "a six-digit code is still what the operator reads out; body: {body}"
    );

    let stored = live_code(&h.loopback).await;
    assert_eq!(
        stored["code"], body["code"],
        "the same live code, read back"
    );
    assert_eq!(
        stored["profile_id"], liz,
        "`pairing_codes.profile_id` is what `verify_handshake` copies onto the device. \
         If it is null on the read-back the route called `issue_pairing_code()` and the \
         whole rung is unattributed, however the issuance response reads. Got: {stored}"
    );
}

/// The backwards-compatible path, and the vacuity control for every "no code
/// was issued" assertion below: issuance really does work in this harness.
#[tokio::test]
async fn a_code_issued_with_no_body_at_all_is_unattributed_as_it_always_was() {
    let h = make_app().await;

    let (status, body) = issue(&h.loopback, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["code"].as_str().is_some(),
        "the CLI and the desktop both POST this route with no body; body: {body}"
    );
    assert!(
        body["profile_id"].is_null(),
        "this harness has NO profiles, so there is no member to infer and the \
         code must belong to nobody; body: {body}"
    );

    let stored = live_code(&h.loopback).await;
    assert_eq!(stored["code"], body["code"]);
    assert!(stored["profile_id"].is_null());
}

// ── The sole-member default ──────────────────────────────────────────────────

/// `issue_pairing_code_for` takes a member, and a caller that passes none leaves every device
/// on the pond paired unattributed. Such a device falls through the paired-device rung on
/// every turn, so `sessions.profile_id` is never written and the summary corpus is
/// unretrievable.
#[tokio::test]
async fn a_sole_member_household_binds_the_code_without_being_asked() {
    let h = make_app().await;
    let jerry = a_member(&h, "Jerry").await;

    // No body at all -- exactly what the CLI and the desktop dashboard send.
    let (status, body) = issue(&h.loopback, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["profile_id"].as_str(),
        Some(jerry.as_str()),
        "one member means one answer to whose device this is, and refusing to \
         write it down does not make the pond safer; body: {body}"
    );

    let stored = live_code(&h.loopback).await;
    assert_eq!(stored["profile_id"].as_str(), Some(jerry.as_str()));
}

/// The escape hatch, and the reason the default above is safe to have. Without it a
/// one-member pond could not pair a guest's phone without that phone becoming the member's,
/// and every turn it sent would inherit an identity nobody claimed.
#[tokio::test]
async fn a_sole_member_household_can_still_pair_a_guests_phone() {
    let h = make_app().await;
    let _jerry = a_member(&h, "Jerry").await;

    let (status, body) = issue(&h.loopback, Some(r#"{"unattributed": true}"#)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["profile_id"].is_null(),
        "the operator asked for a code that binds to nobody and must get one; \
         body: {body}"
    );

    let stored = live_code(&h.loopback).await;
    assert!(stored["profile_id"].is_null());
}

/// Two members is no answer, not a coin flip. Binding to whoever was created
/// first would attribute a phone by row order, which is evidence of nothing.
#[tokio::test]
async fn two_members_still_require_the_operator_to_name_one() {
    let h = make_app().await;
    let _jerry = a_member(&h, "Jerry").await;
    let _liz = a_member(&h, "Liz").await;

    let (status, body) = issue(&h.loopback, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["profile_id"].is_null(),
        "with two members there is nothing to infer; body: {body}"
    );
}

// ── The security argument the capture point rests on ─────────────────────────

/// If this ever passes as a 200, capture-at-issuance has become capture-from-a-
/// client, which is the hole PAI-1 P4 closed one rung lower down.
#[tokio::test]
async fn a_remote_peer_naming_a_member_is_refused_and_mints_nothing() {
    let h = make_app().await;
    let liz = a_member(&h, "Liz").await;

    let (status, body) = issue(&h.remote, Some(&format!(r#"{{"profile_id":"{liz}"}}"#))).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a non-loopback caller must not be able to issue a code at all, let alone \
         one that names a member: `PairedDevice` outranks face and explicit \
         identification, so this would outrank every proof the pond can make. body: {body}"
    );
    assert!(
        live_code(&h.loopback).await["code"].is_null(),
        "the refusal must happen before the code is minted"
    );
}

#[tokio::test]
async fn a_member_who_does_not_exist_is_refused_and_mints_nothing() {
    let h = make_app().await;
    // A real member exists, so the refusal below is about THIS id and not about
    // an empty `profiles` table.
    let _liz = a_member(&h, "Liz").await;

    let (status, body) = issue(&h.loopback, Some(r#"{"profile_id":"ghost"}"#)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "migration 0043's foreign key refuses a code for a member who is not on file; \
         answering 200 would hand the operator a code that attributes a phone to a \
         ghost. body: {body}"
    );
    assert!(
        live_code(&h.loopback).await["code"].is_null(),
        "a refused issuance must leave no row behind"
    );
}

#[tokio::test]
async fn a_blank_member_is_refused_rather_than_stored() {
    let h = make_app().await;

    for blank in ["\"\"", "\"   \"", "\"\\t\""] {
        let (status, body) =
            issue(&h.loopback, Some(&format!(r#"{{"profile_id":{blank}}}"#))).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a blank profile id is neither NULL nor a member: `ON DELETE SET NULL` \
             could never clear it, so it would attribute a device to nobody forever. \
             body for {blank}: {body}"
        );
    }
    assert!(live_code(&h.loopback).await["code"].is_null());
}

/// The typo case, the one that would otherwise be silent. `deny_unknown_fields` turns a
/// misspelt `profileId` into a 400; without it the operator is told the code was issued,
/// the code is unattributed, and nothing surfaces until a paired phone gets no proposals.
#[tokio::test]
async fn a_body_this_route_does_not_understand_is_refused_not_defaulted() {
    let h = make_app().await;
    let liz = a_member(&h, "Liz").await;

    for body in [
        format!(r#"{{"profileId":"{liz}"}}"#),
        format!(r#"{{"profile_id":"{liz}","extra":1}}"#),
        r#"{"profile_id":42}"#.to_string(),
        "not json at all".to_string(),
    ] {
        let (status, resp) = issue(&h.loopback, Some(&body)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "`{body}` must be refused rather than read as an unattributed code; resp: {resp}"
        );
    }
    assert!(
        live_code(&h.loopback).await["code"].is_null(),
        "not one of those bodies may mint a code"
    );
}

// ── The read-back ────────────────────────────────────────────────────────────

/// `GET /handshake/pairing-code` re-displays the live code, and now says whose
/// it is. An operator who cannot see the binding cannot notice a wrong one.
#[tokio::test]
async fn re_displaying_the_code_names_the_member_it_is_bound_to() {
    let h = make_app().await;
    let liz = a_member(&h, "Liz").await;
    let (status, issued) = issue(&h.loopback, Some(&format!(r#"{{"profile_id":"{liz}"}}"#))).await;
    assert_eq!(status, StatusCode::OK);

    let body = live_code(&h.loopback).await;

    assert_eq!(body["code"], issued["code"], "same live code");
    assert_eq!(
        body["profile_id"], liz,
        "the re-display must name the member too; body: {body}"
    );
}
