//! The web search settings routes, against a stand-in for Brave: what each answer from the key
//! check does to the store and the reported status, that the key never comes back, and that every
//! route needs a token.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_api::web_search::WebSearchBackend;
use pond_core::mcp::services::web_search_status::WebSearchStatusBoard;
use pond_core::security::ports::secret::SecretRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY_NAME: &str = "BRAVE_SEARCH_API_KEY";

struct Pond {
    router: axum::Router,
    secrets: Arc<pond_infra::file_secret_repository::FileSecretRepository>,
    settings: Arc<pond_infra::sqlite_settings::SqliteSettingsRepository>,
    _dir: tempfile::TempDir,
}

/// A pond whose key check goes to `brave_base`, with its own status board.
async fn pond(brave_base: &str) -> Pond {
    // The store reads the process environment first; a developer's exported key must not leak in.
    std::env::remove_var(KEY_NAME);
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let secrets = Arc::new(
        pond_infra::file_secret_repository::FileSecretRepository::new(dir.path()).unwrap(),
    );
    let handshake = Arc::new(pond_infra::mock_handshake::MockHandshake::new());
    handshake.add_valid_token("desktop-token".to_string()).await;
    let settings = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    let state = Arc::new(pond_api::AppState {
        secret_repo: Some(secrets.clone()),
        settings_repo: settings.clone(),
        handshake,
        ..state
    });
    let board: &'static WebSearchStatusBoard = Box::leak(Box::new(WebSearchStatusBoard::new()));
    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"))
        .layer(axum::Extension(WebSearchBackend::new(brave_base, board)))
        .layer(MockConnectInfo(SocketAddr::from((
            [192, 168, 1, 44],
            40_000,
        ))));
    Pond {
        router,
        secrets,
        settings,
        _dir: dir,
    }
}

async fn call(
    router: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: bool,
) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if token {
        req = req.header("Authorization", "Bearer desktop-token");
    }
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn state_of(pond: &Pond) -> Value {
    let (status, body) = call(&pond.router, Method::GET, "/api/v1/web-search", None, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn put_key(pond: &Pond, key: &str) -> (StatusCode, String) {
    call(
        &pond.router,
        Method::PUT,
        "/api/v1/web-search/key",
        Some(json!({ "key": key })),
        true,
    )
    .await
}

/// A Brave that answers `status` to exactly one key check made with `key`.
async fn brave_answering(status: u16, key: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(query_param("q", "test"))
        .and(query_param("count", "1"))
        .and(header("X-Subscription-Token", key))
        .and(header("Accept", "application/json"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({"type": "search"})))
        .expect(1)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn with_no_key_web_search_is_off() {
    let pond = pond("http://127.0.0.1:1").await;
    assert_eq!(
        state_of(&pond).await,
        json!({
            "provider": "brave",
            "configured": false,
            "status": "not_configured",
            "local_results": {"enabled": false, "town": null},
        })
    );
}

#[tokio::test]
async fn a_key_brave_accepts_is_stored_and_reported_ok_but_never_returned() {
    let brave = brave_answering(200, "good-key-123").await;
    let pond = pond(&brave.uri()).await;

    let (status, body) = put_key(&pond, "  good-key-123  ").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"configured": true, "status": "ok"})
    );
    assert!(!body.contains("good-key-123"));
    assert_eq!(
        pond.secrets.get(KEY_NAME).await.unwrap().as_deref(),
        Some("good-key-123"),
        "stored trimmed, under the name the tool reads"
    );

    let shown = state_of(&pond).await;
    assert_eq!(
        shown,
        json!({
            "provider": "brave",
            "configured": true,
            "status": "ok",
            "local_results": {"enabled": false, "town": null},
        })
    );
    assert!(!shown.to_string().contains("good-key-123"));
}

#[tokio::test]
async fn a_key_brave_refuses_is_not_stored() {
    for refused in [401, 403] {
        let brave = brave_answering(refused, "bad-key").await;
        let pond = pond(&brave.uri()).await;
        let (status, body) = put_key(&pond, "bad-key").await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{refused}: {body}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"error": "Brave did not accept this key."})
        );
        assert_eq!(pond.secrets.get(KEY_NAME).await.unwrap(), None, "{refused}");
        assert_eq!(state_of(&pond).await["status"], "not_configured");
    }
}

#[tokio::test]
async fn a_refused_replacement_keeps_the_key_already_stored() {
    let brave = brave_answering(401, "bad-key").await;
    let pond = pond(&brave.uri()).await;
    pond.secrets.set(KEY_NAME, "the-old-key").await.unwrap();
    let (status, _) = put_key(&pond, "bad-key").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        pond.secrets.get(KEY_NAME).await.unwrap().as_deref(),
        Some("the-old-key")
    );
}

#[tokio::test]
async fn a_valid_but_spent_key_is_stored_and_reported_as_quota() {
    for spent in [402, 429] {
        let brave = brave_answering(spent, "spent-key").await;
        let pond = pond(&brave.uri()).await;
        let (status, body) = put_key(&pond, "spent-key").await;
        assert_eq!(status, StatusCode::OK, "{spent}: {body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"configured": true, "status": "quota"})
        );
        assert_eq!(
            pond.secrets.get(KEY_NAME).await.unwrap().as_deref(),
            Some("spent-key")
        );
        assert_eq!(state_of(&pond).await["status"], "quota", "{spent}");
    }
}

#[tokio::test]
async fn a_key_saved_while_brave_is_unreachable_is_stored_unverified() {
    // Nothing listens on port 1.
    let pond = pond("http://127.0.0.1:1").await;
    let (status, body) = put_key(&pond, "offline-key").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"configured": true, "status": "unverified"})
    );
    assert_eq!(
        pond.secrets.get(KEY_NAME).await.unwrap().as_deref(),
        Some("offline-key")
    );
    assert_eq!(state_of(&pond).await["status"], "unverified");
}

#[tokio::test]
async fn a_server_error_from_brave_says_nothing_about_the_key() {
    let brave = brave_answering(503, "maybe-key").await;
    let pond = pond(&brave.uri()).await;
    let (status, body) = put_key(&pond, "maybe-key").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["status"],
        "unverified"
    );
}

#[tokio::test]
async fn a_put_without_a_key_is_refused_and_asks_brave_nothing() {
    let brave = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&brave)
        .await;
    let pond = pond(&brave.uri()).await;
    for body in [json!({}), json!({"key": "   "}), json!({"value": "x"})] {
        let (status, text) = call(
            &pond.router,
            Method::PUT,
            "/api/v1/web-search/key",
            Some(body.clone()),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {text}");
    }
}

#[tokio::test]
async fn removing_the_key_turns_web_search_off() {
    let brave = brave_answering(200, "good-key").await;
    let pond = pond(&brave.uri()).await;
    assert_eq!(put_key(&pond, "good-key").await.0, StatusCode::OK);

    let (status, body) = call(
        &pond.router,
        Method::DELETE,
        "/api/v1/web-search/key",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(pond.secrets.get(KEY_NAME).await.unwrap(), None);
    assert_eq!(
        state_of(&pond).await,
        json!({
            "provider": "brave",
            "configured": false,
            "status": "not_configured",
            "local_results": {"enabled": false, "town": null},
        })
    );
}

#[tokio::test]
async fn every_route_needs_a_token() {
    let pond = pond("http://127.0.0.1:1").await;
    for (method, uri, body) in [
        (Method::GET, "/api/v1/web-search", None),
        (
            Method::PUT,
            "/api/v1/web-search/key",
            Some(json!({"key": "k"})),
        ),
        (Method::DELETE, "/api/v1/web-search/key", None),
    ] {
        let (status, text) = call(&pond.router, method.clone(), uri, body, false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {text}");
    }
    assert_eq!(
        pond.secrets.get(KEY_NAME).await.unwrap(),
        None,
        "a refused PUT stored nothing"
    );
}

#[tokio::test]
async fn a_key_no_header_can_carry_is_refused_unchecked_and_unstored() {
    let brave = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&brave)
        .await;
    let pond = pond(&brave.uri()).await;
    for key in ["two\nlines", "curly\u{201c}quote", "inner space"] {
        let (status, body) = put_key(&pond, key).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key:?}: {body}");
        assert!(!body.contains(key), "{body}");
    }
    assert_eq!(pond.secrets.get(KEY_NAME).await.unwrap(), None);
}

/// A redirect from Brave's host is an answer, not a hop: the key's header goes nowhere else.
#[tokio::test]
async fn the_key_check_never_follows_a_redirect() {
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&elsewhere)
        .await;
    let brave = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/collect", elsewhere.uri()).as_str()),
        )
        .expect(1)
        .mount(&brave)
        .await;
    let pond = pond(&brave.uri()).await;

    let (status, body) = put_key(&pond, "redirected-key").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"configured": true, "status": "unverified"})
    );
}

/// Local places are off until switched on through the settings route, and the town the status
/// reports is the stored place name's first part, whether or not it is being sent.
#[tokio::test]
async fn local_places_are_reported_off_by_default_and_follow_the_settings_switch() {
    let pond = pond("http://127.0.0.1:1").await;
    pond.settings
        .set_key("weather_location_name", "Nairobi, Kenya".to_string())
        .await
        .unwrap();
    assert_eq!(
        state_of(&pond).await["local_results"],
        json!({"enabled": false, "town": "Nairobi"})
    );

    let (status, body) = call(
        &pond.router,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"web_search_local_results": true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        state_of(&pond).await["local_results"],
        json!({"enabled": true, "town": "Nairobi"})
    );
    assert!(pond.settings.get().await.unwrap().web_search_local_results);

    let (status, body) = call(
        &pond.router,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"web_search_local_results": false})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        state_of(&pond).await["local_results"],
        json!({"enabled": false, "town": "Nairobi"})
    );
}
