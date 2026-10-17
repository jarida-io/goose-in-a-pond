//! #365, onboarding half: `POST /onboard/complete` refuses an empty `chat_model`, so the model
//! routes that list, fetch and pick it must answer a paired device before onboarding, and never
//! a caller with no token. The rest of `/models` stays behind the onboarding guard.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::ports::settings::SettingsRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

const CHAT_MODEL: &str = "gemma-onboarding";

struct Harness {
    loopback: axum::Router,
    lan: axum::Router,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    _dir: tempfile::TempDir,
}

/// A fresh pond mid-onboarding, its chat model in the catalogue and already on disk, so nothing
/// here reaches the network.
async fn fresh_pond() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    state.skip_onboarding = false;
    // No data directory, so the catalogue's background disk scan cannot mark the model missing.
    state.data_dir = None;
    state.settings_repo = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    let models = Arc::new(
        pond_infra::sqlite_model_repository::SqliteModelRepository::new(state.db.system.clone()),
    );
    models.upsert(&chat_model()).await.unwrap();
    state.model_repo = Some(models);
    let settings = state.settings_repo.clone();
    let state: Arc<AppState> = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let lan = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 50],
        40_000,
    ))));
    Harness {
        loopback,
        lan,
        settings,
        handshake,
        _dir: dir,
    }
}

fn chat_model() -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Gguf, CHAT_MODEL),
        category: ModelCategory::Gguf,
        name: CHAT_MODEL.to_string(),
        filename: Some(format!("{CHAT_MODEL}.gguf")),
        description: "the onboarding chat model".into(),
        size_mb: 1000,
        // No URL: should the model ever read as missing, nothing here can fetch it.
        url: None,
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: Some("chat".into()),
        context_length: None,
        quantization: None,
        asr_language: None,
        asr_size: None,
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: true,
        is_custom: false,
    }
}

/// Pairs over loopback: pairing needs a peer on one of the host's own LAN subnets, which a fixed
/// address only is on some machines, while the model routes take the token from any peer.
async fn paired(h: &Harness) -> String {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, "phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session_token"].as_str().unwrap().to_string()
}

async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The five routes, each with a body that would succeed or fail on its own merits.
fn the_five() -> Vec<(Method, String, Option<Value>)> {
    vec![
        (Method::GET, "/api/v1/models".into(), None),
        (Method::GET, "/api/v1/models/download/progress".into(), None),
        (
            Method::POST,
            "/api/v1/models/download/control".into(),
            Some(json!({"action": "pause", "model_id": "gguf/nothing-downloading"})),
        ),
        (
            Method::POST,
            format!("/api/v1/models/gguf/{CHAT_MODEL}/download"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/models/gguf/{CHAT_MODEL}/activate"),
            Some(json!({"role": "chat"})),
        ),
    ]
}

#[tokio::test]
async fn a_paired_phone_chooses_the_chat_model_before_onboarding_and_can_then_finish() {
    let h = fresh_pond().await;
    let token = paired(&h).await;

    let (status, models) = send(&h.lan, Method::GET, "/api/v1/models", None, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{models}");
    assert!(
        models.to_string().contains(CHAT_MODEL),
        "the catalogue the phone sees lists the chat model: {models}"
    );
    let (status, progress) = send(
        &h.lan,
        Method::GET,
        "/api/v1/models/download/progress",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{progress}");
    let (status, fetched) = send(
        &h.lan,
        Method::POST,
        &format!("/api/v1/models/gguf/{CHAT_MODEL}/download"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched["status"], "already_downloaded");
    let (status, control) = send(
        &h.lan,
        Method::POST,
        "/api/v1/models/download/control",
        Some(json!({"action": "pause", "model_id": "gguf/nothing-downloading"})),
        Some(&token),
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN, "{control}");
    assert_ne!(status, StatusCode::UNAUTHORIZED, "{control}");

    let (status, activated) = send(
        &h.lan,
        Method::POST,
        &format!("/api/v1/models/gguf/{CHAT_MODEL}/activate"),
        Some(json!({"role": "chat"})),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(
        h.settings.get().await.unwrap().chat_model,
        CHAT_MODEL,
        "activating a chat model is what fills the field completion checks"
    );

    let (status, saved) = send(
        &h.lan,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"user_name": "Jerry", "assistant_name": "Goose", "timezone": "UTC"})),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, done) = send(
        &h.lan,
        Method::POST,
        "/api/v1/onboard/complete",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the phone can finish setup now: {done}"
    );

    // After onboarding they behave as they always have.
    for (method, path, body) in the_five() {
        let (status, answer) = send(&h.lan, method.clone(), &path, body, Some(&token)).await;
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{method} {path} after onboarding: {status} {answer}"
        );
    }
}

#[tokio::test]
async fn the_model_routes_never_answer_a_caller_with_no_token() {
    let h = fresh_pond().await;
    let token = paired(&h).await;
    let check = |stage: &'static str| {
        let (loopback, lan) = (h.loopback.clone(), h.lan.clone());
        async move {
            for (method, path, body) in the_five() {
                for (peer, router) in [("host", &loopback), ("LAN", &lan)] {
                    let (status, _) = send(router, method.clone(), &path, body.clone(), None).await;
                    assert_eq!(
                        status,
                        StatusCode::UNAUTHORIZED,
                        "{method} {path} answered a tokenless {peer} caller {stage}"
                    );
                }
            }
        }
    };
    check("before onboarding").await;

    let mut settings = h.settings.get().await.unwrap();
    settings.user_name = "Jerry".into();
    settings.assistant_name = "Goose".into();
    settings.timezone = "UTC".into();
    settings.chat_model = CHAT_MODEL.into();
    h.settings.update(&settings).await.unwrap();
    let (status, _) = send(
        &h.lan,
        Method::POST,
        "/api/v1/onboard/complete",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    check("after onboarding").await;
}

#[tokio::test]
async fn the_other_model_routes_still_wait_for_onboarding() {
    let h = fresh_pond().await;
    let token = paired(&h).await;
    for (method, path) in [
        (Method::DELETE, format!("/api/v1/models/gguf/{CHAT_MODEL}")),
        (Method::POST, "/api/v1/models/scan".to_string()),
        (Method::POST, "/api/v1/models/cleanup".to_string()),
        (Method::GET, "/api/v1/models/disk-usage".to_string()),
        (
            Method::GET,
            "/api/v1/models/search/gguf?q=gemma".to_string(),
        ),
        (Method::POST, "/api/v1/models/ollama/pull".to_string()),
        (Method::POST, "/api/v1/models/download/url".to_string()),
    ] {
        let (status, body) =
            send(&h.lan, method.clone(), &path, Some(json!({})), Some(&token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        assert_eq!(body["error"], "onboarding_required", "{method} {path}");
    }
}
