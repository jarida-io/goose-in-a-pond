//! Connecting a member's Uber account from the pond's desktop, through a stand-in for Jarida's
//! credentials service: who may start it, the round trip, and where the tokens end up.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::security::ports::secret::SecretRepository;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static CREDENTIAL: std::sync::LazyLock<pond_api::host_guard::HostCredential> =
    std::sync::LazyLock::new(pond_api::host_guard::HostCredential::generate);

struct Pond {
    loopback: axum::Router,
    remote: axum::Router,
    /// A paired phone on the companion listener: a valid bearer, no host credential.
    phone: axum::Router,
    secrets: Arc<pond_infra::file_secret_repository::FileSecretRepository>,
    member: String,
    api_port: u16,
    state: Arc<pond_api::AppState>,
    _dir: tempfile::TempDir,
}

/// `POND_CREDENTIALS_URL` is process-global, so a test that sets it takes a turn for its whole run.
static CREDENTIALS_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Points the pond at a credentials service, and back at none when dropped.
struct CredentialsAt;

impl CredentialsAt {
    fn set(url: &str) -> CredentialsAt {
        std::env::set_var("POND_CREDENTIALS_URL", url);
        CredentialsAt
    }
}

impl Drop for CredentialsAt {
    fn drop(&mut self) {
        // `off`, never unset: unset means Jarida's real service, which no test may call.
        std::env::set_var("POND_CREDENTIALS_URL", "off");
    }
}

async fn pond() -> Pond {
    if std::env::var_os("POND_CREDENTIALS_URL").is_none() {
        std::env::set_var("POND_CREDENTIALS_URL", "off");
    }
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let secrets = Arc::new(
        pond_infra::file_secret_repository::FileSecretRepository::new(dir.path()).unwrap(),
    );
    let member = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Liz".to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id;
    let api_port = state.api_port;
    let handshake = Arc::new(pond_infra::mock_handshake::MockHandshake::new());
    handshake.add_valid_token("desktop-token".to_string()).await;
    let state = Arc::new(pond_api::AppState {
        secret_repo: Some(secrets.clone()),
        // The desktop's own paired token; the real handshake's pairing is not what is under test.
        handshake,
        ..state
    });
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let router = |ip: [u8; 4]| {
        build_router(state.clone(), dist.clone())
            .layer(axum::Extension(CREDENTIAL.clone()))
            .layer(MockConnectInfo(SocketAddr::from((ip, 40_000))))
    };
    Pond {
        loopback: router([127, 0, 0, 1]),
        remote: router([192, 168, 1, 44]),
        phone: pond_api::build_companion_router(state.clone()).layer(MockConnectInfo(
            SocketAddr::from(([192, 168, 1, 45], 40_000)),
        )),
        secrets,
        member,
        api_port,
        state,
        _dir: dir,
    }
}

async fn call(
    router: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    with_credential: bool,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer desktop-token");
    if with_credential {
        req = req.header(pond_api::host_guard::CREDENTIAL_HEADER, CREDENTIAL.as_str());
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

async fn credentials_service() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/uber/client"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"client_id": "uber-app"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uber/token"))
        .and(body_partial_json(json!({"code": "good-code"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 2592000
        })))
        .mount(&server)
        .await;
    server
}

/// Only the pond's own desktop, with the host credential, may connect an account.
#[tokio::test]
async fn a_phone_or_a_page_without_the_host_credential_cannot_start_a_sign_in() {
    let p = pond().await;
    let body = json!({"profile_id": p.member});
    let (status, _) = call(
        &p.remote,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(body),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&p.remote, Method::GET, "/api/v1/uber/accounts", None, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_sign_in_is_only_for_a_real_member() {
    let p = pond().await;
    let (status, _) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(json!({"profile_id": "nobody"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The generic secrets API never reaches a member's Uber sign-in, whatever bearer it is given,
/// while ordinary keys go on working.
#[tokio::test]
async fn the_secrets_api_cannot_reach_a_members_uber_sign_in() {
    let p = pond().await;
    let key = format!("UBER_REFRESH_TOKEN:{}", p.member);
    let lowercase = key.to_lowercase();
    p.secrets.set(&key, "refresh-1").await.unwrap();

    for router in [&p.phone, &p.loopback] {
        for k in [&key, &lowercase] {
            for (method, uri, body) in [
                (Method::GET, format!("/api/v1/secrets/{k}/exists"), None),
                (
                    Method::PUT,
                    format!("/api/v1/secrets/{k}"),
                    Some(json!({"value": "stolen"})),
                ),
                (Method::DELETE, format!("/api/v1/secrets/{k}"), None),
                (
                    Method::POST,
                    "/api/v1/extensions/music/secrets".to_string(),
                    Some(json!({ k.as_str(): "stolen" })),
                ),
            ] {
                let (status, reply) = call(router, method.clone(), &uri, body, false).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {reply}");
                assert_eq!(
                    serde_json::from_str::<Value>(&reply).unwrap()["code"],
                    "secret_reserved",
                    "{method} {uri}"
                );
            }
        }
        let (status, list) = call(router, Method::GET, "/api/v1/secrets", None, false).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert!(!list.to_uppercase().contains("UBER_"), "{list}");
    }
    assert_eq!(
        p.secrets.get(&key).await.unwrap().as_deref(),
        Some("refresh-1")
    );
    assert_eq!(p.secrets.get(&lowercase).await.unwrap(), None);

    // An ordinary key, from the phone: stored, seen, listed and deleted.
    let ordinary = "/api/v1/secrets/GIAP_TEST_ORDINARY_SECRET";
    let (status, reply) = call(
        &p.phone,
        Method::PUT,
        ordinary,
        Some(json!({"value": "ordinary-value"})),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (_, exists) = call(
        &p.phone,
        Method::GET,
        &format!("{ordinary}/exists"),
        None,
        false,
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&exists).unwrap()["exists"],
        true
    );
    let (_, list) = call(&p.phone, Method::GET, "/api/v1/secrets", None, false).await;
    assert!(list.contains("GIAP_TEST_ORDINARY_SECRET"), "{list}");
    let (status, _) = call(&p.phone, Method::DELETE, ordinary, None, false).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        p.secrets.get("GIAP_TEST_ORDINARY_SECRET").await.unwrap(),
        None
    );
}

/// The whole round trip.
#[tokio::test]
async fn a_member_connects_uber_and_the_tokens_are_kept_for_them_alone() {
    let _turn = CREDENTIALS_TURN.lock().await;
    let service = credentials_service().await;
    let _at = CredentialsAt::set(&service.uri());
    let p = pond().await;

    // 1. The desktop starts the sign-in and gets Uber's page.
    let (status, body) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(json!({"profile_id": p.member})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let started: Value = serde_json::from_str(&body).unwrap();
    let auth_url = started["auth_url"].as_str().unwrap();
    let nonce = started["state"].as_str().unwrap();
    assert!(auth_url.starts_with("https://auth.uber.com/oauth/v2/authorize?client_id=uber-app"));
    let callback = format!(
        "http%3A%2F%2F127.0.0.1%3A{}%2Fapi%2Fv1%2Foauth%2Fcallback",
        p.api_port
    );
    assert!(auth_url.contains(&callback), "{auth_url}");

    // 2. Uber sends the browser back with a code.
    let (status, page) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/callback?code=good-code&state={nonce}"),
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Uber is connected"), "{page}");

    // 3. The tokens are this member's, and the desktop sees the connection and the outcome.
    let key = format!("UBER_REFRESH_TOKEN:{}", p.member);
    assert_eq!(
        p.secrets.get(&key).await.unwrap().as_deref(),
        Some("refresh-1")
    );
    let (_, list) = call(
        &p.loopback,
        Method::GET,
        "/api/v1/uber/accounts",
        None,
        true,
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&list).unwrap()["connected"],
        json!([p.member])
    );
    let (_, outcome) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/status/{nonce}"),
        None,
        false,
    )
    .await;
    assert!(
        outcome.contains("completed") || outcome.contains("Completed"),
        "{outcome}"
    );

    // 4. A replayed return finds no sign-in to finish.
    let (_, replay) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/callback?code=good-code&state={nonce}"),
        None,
        false,
    )
    .await;
    assert!(!replay.contains("Uber is connected"), "{replay}");

    // 5. Disconnecting forgets the tokens on this pond.
    let (status, _) = call(
        &p.loopback,
        Method::DELETE,
        &format!("/api/v1/uber/accounts/{}", p.member),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(p.secrets.get(&key).await.unwrap(), None);

    // 6. A cancelled sign-in records why and stores nothing.
    let (_, body) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(json!({"profile_id": p.member})),
        true,
    )
    .await;
    let nonce: Value = serde_json::from_str(&body).unwrap();
    let nonce = nonce["state"].as_str().unwrap();
    let (_, page) = call(
        &p.loopback,
        Method::GET,
        &format!(
            "/api/v1/oauth/callback?error=access_denied\
             &error_description=The+user+denied+the+request&state={nonce}"
        ),
        None,
        false,
    )
    .await;
    assert!(page.contains("Uber is not connected"), "{page}");
    assert!(
        page.contains("access_denied") && page.contains("The user denied the request"),
        "{page}"
    );
    let (_, outcome) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/status/{nonce}"),
        None,
        false,
    )
    .await;
    let outcome = parse(&outcome);
    assert_eq!(outcome["status"], "failed");
    assert!(
        outcome["error"].as_str().unwrap().contains("access_denied"),
        "{outcome}"
    );
    assert_eq!(p.secrets.get(&key).await.unwrap(), None);
}

/// Through an SSH tunnel the desktop's origin is the tunnel's port; Uber sends the browser back
/// there, and the exchange sends that same URI rather than the port the pond bound.
#[tokio::test]
async fn a_tunnelled_sign_in_returns_through_the_tunnel_and_exchanges_with_that_uri() {
    let _turn = CREDENTIALS_TURN.lock().await;
    let service = credentials_service().await;
    let _at = CredentialsAt::set(&service.uri());
    let p = pond().await;
    let tunnelled = "http://127.0.0.1:9080/api/v1/oauth/callback";

    let (status, body) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(json!({"profile_id": p.member, "origin": "http://localhost:9080"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let started = parse(&body);
    let auth_url = started["auth_url"].as_str().unwrap();
    assert!(
        auth_url.contains(&format!("redirect_uri={}", urlencoding::encode(tunnelled))),
        "{auth_url}"
    );

    let nonce = started["state"].as_str().unwrap();
    let (status, page) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/callback?code=good-code&state={nonce}"),
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Uber is connected"), "{page}");

    let exchanged: Vec<Value> = service
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/uber/token")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(exchanged.len(), 1, "{exchanged:?}");
    assert_eq!(exchanged[0]["redirect_uri"], tunnelled);
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

async fn uber_keys(p: &Pond) -> Vec<String> {
    let mut keys: Vec<String> = p
        .secrets
        .list_keys()
        .await
        .unwrap()
        .into_iter()
        .filter(|k| k.starts_with("UBER_"))
        .collect();
    keys.sort();
    keys
}

async fn start_sign_in(p: &Pond) -> String {
    let (status, body) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(json!({"profile_id": p.member})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    parse(&body)["state"].as_str().unwrap().to_string()
}

async fn delete_member(p: &Pond) -> Value {
    let (status, body) = call(
        &p.loopback,
        Method::DELETE,
        &format!("/api/v1/profiles/{}", p.member),
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    parse(&body)
}

#[tokio::test]
async fn deleting_a_member_forgets_their_uber_sign_in() {
    let _turn = CREDENTIALS_TURN.lock().await;
    let service = credentials_service().await;
    let _at = CredentialsAt::set(&service.uri());
    let p = pond().await;
    // The only profile in an empty household is its owner, and the last owner can't be deleted
    // -- seed a second owner so deleting the member isn't refused on that ground.
    {
        use pond_core::user_data::domain::profile::Role;
        let jerry = p
            .state
            .profile_repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "Y".to_string(),
            })
            .await
            .unwrap();
        p.state
            .profile_repo
            .set_role(&jerry.id, Role::Owner, None)
            .await
            .unwrap();
    }

    let nonce = start_sign_in(&p).await;
    let (_, page) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/callback?code=good-code&state={nonce}"),
        None,
        false,
    )
    .await;
    assert!(page.contains("Uber is connected"), "{page}");
    assert_eq!(uber_keys(&p).await.len(), 3);

    let deleted = delete_member(&p).await;
    assert_eq!(deleted["deleted"]["uber_accounts"], 1, "{deleted}");
    assert_eq!(uber_keys(&p).await, Vec::<String>::new());
}

/// The member is removed while Uber's code is still being exchanged.
#[tokio::test]
async fn a_sign_in_that_outlives_its_member_keeps_nothing() {
    let _turn = CREDENTIALS_TURN.lock().await;
    let service = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/uber/client"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"client_id": "uber-app"})))
        .mount(&service)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uber/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 3600
                }))
                .set_delay(std::time::Duration::from_secs(1)),
        )
        .mount(&service)
        .await;
    let _at = CredentialsAt::set(&service.uri());
    let p = pond().await;
    // The only profile in an empty household is its owner, and the last owner can't be deleted
    // -- seed a second owner so deleting the member isn't refused on that ground.
    {
        use pond_core::user_data::domain::profile::Role;
        let jerry = p
            .state
            .profile_repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "Y".to_string(),
            })
            .await
            .unwrap();
        p.state
            .profile_repo
            .set_role(&jerry.id, Role::Owner, None)
            .await
            .unwrap();
    }

    let nonce = start_sign_in(&p).await;
    let returning = tokio::spawn({
        let router = p.loopback.clone();
        let uri = format!("/api/v1/oauth/callback?code=good-code&state={nonce}");
        async move { call(&router, Method::GET, &uri, None, false).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    delete_member(&p).await;

    let (_, page) = returning.await.unwrap();
    assert!(page.contains("no longer in the household"), "{page}");
    assert_eq!(uber_keys(&p).await, Vec::<String>::new());
}

#[tokio::test]
async fn with_sign_in_turned_off_a_connection_is_still_listed_and_forgotten() {
    let _turn = CREDENTIALS_TURN.lock().await;
    let _at = CredentialsAt::set("off");
    let p = pond().await;
    for prefix in [
        "UBER_ACCESS_TOKEN:",
        "UBER_REFRESH_TOKEN:",
        "UBER_TOKEN_EXPIRES_AT:",
    ] {
        p.secrets
            .set(&format!("{prefix}{}", p.member), "1")
            .await
            .unwrap();
    }

    let (status, list) = call(
        &p.loopback,
        Method::GET,
        "/api/v1/uber/accounts",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(parse(&list)["connected"], json!([p.member]));
    let (status, body) = call(
        &p.loopback,
        Method::DELETE,
        &format!("/api/v1/uber/accounts/{}", p.member),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(uber_keys(&p).await, Vec::<String>::new());

    let (status, body) = call(
        &p.loopback,
        Method::POST,
        "/api/v1/uber/accounts/connect",
        Some(json!({"profile_id": p.member})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(parse(&body)["code"], "uber_sign_in_off");
}

#[tokio::test]
async fn the_list_names_only_people_still_in_the_household() {
    let p = pond().await;
    p.secrets
        .set(&format!("UBER_REFRESH_TOKEN:{}", p.member), "r")
        .await
        .unwrap();
    p.secrets
        .set("UBER_REFRESH_TOKEN:someone-no-longer-here", "r")
        .await
        .unwrap();

    let (status, list) = call(
        &p.loopback,
        Method::GET,
        "/api/v1/uber/accounts",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(parse(&list)["connected"], json!([p.member]));
}

#[tokio::test]
async fn a_sign_in_left_unfinished_for_ten_minutes_expires() {
    let _turn = CREDENTIALS_TURN.lock().await;
    let service = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/uber/client"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"client_id": "uber-app"})))
        .mount(&service)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/uber/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 3600
        })))
        .expect(0)
        .mount(&service)
        .await;
    let _at = CredentialsAt::set(&service.uri());
    let p = pond().await;

    let nonce = start_sign_in(&p).await;
    p.state
        .oauth_state
        .write()
        .await
        .get_mut(&nonce)
        .expect("the sign-in is pending")
        .created_at -= pond_api::oauth_callback::SESSION_TTL + std::time::Duration::from_secs(1);

    let (_, outcome) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/status/{nonce}"),
        None,
        false,
    )
    .await;
    let outcome = parse(&outcome);
    assert_eq!(outcome["status"], "failed", "{outcome}");
    assert!(
        outcome["error"].as_str().unwrap().contains("expired"),
        "{outcome}"
    );

    let (_, page) = call(
        &p.loopback,
        Method::GET,
        &format!("/api/v1/oauth/callback?code=good-code&state={nonce}"),
        None,
        false,
    )
    .await;
    assert!(!page.contains("Uber is connected"), "{page}");
    assert_eq!(uber_keys(&p).await, Vec::<String>::new());
}
