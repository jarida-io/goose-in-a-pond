//! The P3 wire contract over HTTP (`docs/architecture/pond-networking.md`, "Wire contract over
//! the LAN"): the setup credential on `pake/init`, its refusals, and the spoken pairing code.
//! Every peer is a `MockConnectInfo`, so nothing here depends on the host's real interfaces.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use pond_api::spoken_code::SpokenCodes;
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::network::mocks::mock_code_announcer::MockCodeAnnouncer;
use pond_core::network::ports::setup_code_store::SetupCodeStore;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::security::domain::event::{AttributeValue, EventCategory, EventQuery};
use pond_core::security::ports::event_log::EventLog;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::file_setup_code_store::FileSetupCodeStore;
use pond_infra::pairing_cpace::{session_id, setup_channel_identifier, Responder};
use pond_infra::sqlite_event_log::SqliteEventLog;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    tailnet: axum::Router,
    store: Arc<FileSetupCodeStore>,
    code: String,
    announcer: Arc<MockCodeAnnouncer>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    event_log: Arc<SqliteEventLog>,
    _dir: tempfile::TempDir,
}

/// A fresh pond with its setup record made and the claim service and a mock speaker wired.
async fn harness() -> Harness {
    harness_with(|_| {}).await
}

async fn harness_with(adjust: impl FnOnce(&mut AppState)) -> Harness {
    let pond_api::test_support::TestState { mut state, dir, .. } =
        pond_api::test_support::app_state().await;
    let store = Arc::new(FileSetupCodeStore::new(dir.path()));
    let code = store
        .create_if_missing()
        .await
        .unwrap()
        .code
        .digits()
        .to_string();
    let claims = Arc::new(SetupClaimService::new(store.clone()));
    let handshake = Arc::new(
        pond_infra::sqlite_handshake::SqliteHandshakeAdapter::new(
            state.db.system.clone(),
            Some(TEST_SPKI_PIN.to_string()),
        )
        .with_setup_claims(claims.clone()),
    );
    let announcer = Arc::new(MockCodeAnnouncer::new());
    let event_log = Arc::new(SqliteEventLog::new(state.db.logs.clone()));
    state.handshake = handshake;
    state.setup_claims = Some(claims);
    state.spoken_codes = Arc::new(SpokenCodes::new(Some(announcer.clone())));
    state.event_log = Some(event_log.clone());
    adjust(&mut state);
    let settings = state.settings_repo.clone();
    let state = Arc::new(state);

    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let tailnet = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [100, 64, 0, 44],
        40_000,
    ))));
    Harness {
        loopback,
        tailnet,
        store,
        code,
        announcer,
        settings,
        event_log,
        _dir: dir,
    }
}

/// Status, `Retry-After`, the raw body text (for scanning) and the body as JSON.
async fn post(router: &axum::Router, path: &str, body: Option<Value>) -> Answer {
    let request = Request::builder().method(Method::POST).uri(path);
    let request = match body {
        Some(body) => request
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Answer {
        status,
        retry_after,
        json: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

struct Answer {
    status: StatusCode,
    retry_after: Option<String>,
    text: String,
    json: Value,
}

fn init_body(client_id: &str, nonce: &[u8], credential: Option<Value>) -> Value {
    let mut body = json!({
        "client_id": client_id, "client_type": "gotg", "client_version": "test",
        "channel_binding": TEST_SPKI_PIN, "client_nonce": B64.encode(nonce),
    });
    if let Some(credential) = credential {
        body["credential"] = credential;
    }
    body
}

/// The longest run of ASCII digits in `text`; a pairing code is six.
fn longest_digit_run(text: &str) -> usize {
    text.split(|c: char| !c.is_ascii_digit())
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// A whole setup exchange over the router, the way goose-on-the-go runs it.
async fn claim_over_http(h: &Harness, code: &str, client_id: &str) -> Answer {
    let nonce = [0x33u8; 32];
    let init = post(
        &h.loopback,
        "/api/v1/handshake/pake/init",
        Some(init_body(client_id, &nonce, Some(json!("setup")))),
    )
    .await;
    assert_eq!(init.status, StatusCode::OK, "{}", init.text);
    let field = |name: &str| B64.decode(init.json[name].as_str().unwrap()).unwrap();
    let phone = Responder::respond(
        code.as_bytes(),
        &setup_channel_identifier(TEST_SPKI_PIN, client_id),
        &session_id(&field("challenge"), &nonce),
        &field("share"),
    )
    .unwrap();
    let confirm: String = phone
        .confirmation()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    post(
        &h.loopback,
        "/api/v1/handshake/pake/verify",
        Some(json!({
            "challenge_id": init.json["challenge_id"], "device_name": client_id,
            "share": B64.encode(phone.share()), "confirm": confirm,
        })),
    )
    .await
}

async fn audited_method(h: &Harness, device: &str) -> Option<AttributeValue> {
    let events = h
        .event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap();
    events
        .iter()
        .find(|e| {
            e.action == "auth.device_paired"
                && e.attributes.get("device_name") == Some(&AttributeValue::Text(device.into()))
        })
        .and_then(|e| e.attributes.get("method").cloned())
}

#[tokio::test]
async fn an_unknown_credential_is_refused_by_name_not_as_a_bad_body() {
    let h = harness().await;
    for credential in [json!("token"), json!("Setup"), json!(null), json!(5)] {
        let answer = post(
            &h.loopback,
            "/api/v1/handshake/pake/init",
            Some(init_body("phone", &[1u8; 32], Some(credential.clone()))),
        )
        .await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{credential}");
        assert_eq!(answer.json, json!({"error": "invalid_credential"}));
    }
    // A body that is wrong elsewhere is still the generic answer.
    let answer = post(
        &h.loopback,
        "/api/v1/handshake/pake/init",
        Some(json!({"credential": "setup"})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.json, json!({"error": "invalid request body"}));
}

#[tokio::test]
async fn a_phone_claims_the_pond_with_its_card_and_the_audit_names_the_setup_method() {
    let h = harness().await;
    let answer = claim_over_http(&h, &h.code, "card-phone").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json["accepted"], true, "{}", answer.text);
    assert!(answer.json["session_token"].is_string());
    assert!(
        !answer.text.contains(&h.code),
        "the setup code never crosses the wire"
    );
    assert!(h.store.load().await.unwrap().unwrap().claimed_at.is_some());
    assert_eq!(
        audited_method(&h, "card-phone").await,
        Some(AttributeValue::Text("cpace-setup".into()))
    );
}

#[tokio::test]
async fn a_claimed_pond_answers_409_setup_code_unavailable() {
    let h = harness().await;
    h.store.mark_claimed(chrono::Utc::now()).await.unwrap();
    let answer = post(
        &h.loopback,
        "/api/v1/handshake/pake/init",
        Some(init_body("phone", &[1u8; 32], Some(json!("setup")))),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(answer.json, json!({"error": "setup_code_unavailable"}));
}

#[tokio::test]
async fn a_pond_with_no_setup_record_wired_answers_409_too() {
    let plain = pond_api::test_support::app_state().await;
    let router = build_router(Arc::new(plain.state), "pond-desktop/dist".into())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let answer = post(
        &router,
        "/api/v1/handshake/pake/init",
        Some(init_body("phone", &[1u8; 32], Some(json!("setup")))),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(answer.json, json!({"error": "setup_code_unavailable"}));
}

#[tokio::test]
async fn too_many_setup_attempts_answer_429_with_retry_after_in_header_and_body() {
    let h = harness().await;
    for attempt in 0..pond_core::network::services::setup_guess_throttle::FREE_SETUP_FAILURES {
        let answer = post(
            &h.loopback,
            "/api/v1/handshake/pake/init",
            Some(init_body(
                "phone",
                &[attempt as u8; 32],
                Some(json!("setup")),
            )),
        )
        .await;
        assert_eq!(answer.status, StatusCode::OK, "attempt {attempt} is free");
    }
    let answer = post(
        &h.loopback,
        "/api/v1/handshake/pake/init",
        Some(init_body("phone", &[9u8; 32], Some(json!("setup")))),
    )
    .await;
    assert_eq!(answer.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(answer.json["error"], "setup_code_cooling");
    let seconds = answer.json["retry_after"].as_u64().expect("whole seconds");
    assert!((1..=30).contains(&seconds), "{seconds}");
    assert_eq!(answer.retry_after, Some(seconds.to_string()));
}

#[tokio::test]
async fn an_absent_credential_still_pairs_with_a_pairing_code_and_claims_the_pond() {
    let h = harness().await;
    let issued = post(&h.loopback, "/api/v1/handshake/spoken-code", None).await;
    assert_eq!(issued.status, StatusCode::ACCEPTED, "{}", issued.text);
    let code = wait_for_announcement(&h).await;
    let (status, body) = pair_phone(&h.loopback, &code, "spoken-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], true, "{body}");
    assert!(
        h.store.load().await.unwrap().unwrap().claimed_at.is_some(),
        "a pairing-code pairing claims the pond too"
    );
    assert_eq!(
        audited_method(&h, "spoken-phone").await,
        Some(AttributeValue::Text("cpace".into()))
    );
}

async fn wait_for_announcement(h: &Harness) -> String {
    for _ in 0..200 {
        if let Some(code) = h.announcer.announced().last() {
            return code.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pond never read a code aloud");
}

#[tokio::test]
async fn the_spoken_code_is_read_aloud_and_never_in_the_answer() {
    let h = harness().await;
    let answer = post(&h.loopback, "/api/v1/handshake/spoken-code", None).await;
    assert_eq!(answer.status, StatusCode::ACCEPTED);
    assert_eq!(answer.json["spoken"], true);
    let expires_at = answer.json["expires_at"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(expires_at).is_ok());

    let code = wait_for_announcement(&h).await;
    assert_eq!(code.len(), 6);
    assert!(!answer.text.contains(&code));
    assert!(
        longest_digit_run(&answer.text) < 6,
        "no six-digit run may appear in the answer: {}",
        answer.text
    );
}

#[tokio::test]
async fn a_second_spoken_code_within_thirty_seconds_is_limited_with_retry_after() {
    let h = harness().await;
    assert_eq!(
        post(&h.loopback, "/api/v1/handshake/spoken-code", None)
            .await
            .status,
        StatusCode::ACCEPTED
    );
    let answer = post(&h.loopback, "/api/v1/handshake/spoken-code", None).await;
    assert_eq!(answer.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(answer.json["error"], "spoken_code_limited");
    let seconds = answer.json["retry_after"].as_u64().unwrap();
    assert!((1..=30).contains(&seconds), "{seconds}");
    assert_eq!(answer.retry_after, Some(seconds.to_string()));
    assert!(longest_digit_run(&answer.text) < 6, "{}", answer.text);
}

#[tokio::test]
async fn a_household_that_turned_spoken_codes_off_gets_403_and_hears_nothing() {
    let h = harness_with(|state| {
        state.settings_repo = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
            state.db.system.clone(),
        ));
    })
    .await;
    let mut settings = h.settings.get().await.unwrap();
    settings.spoken_pairing_codes = false;
    h.settings.update(&settings).await.unwrap();

    let answer = post(&h.loopback, "/api/v1/handshake/spoken-code", None).await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert_eq!(answer.json, json!({"error": "spoken_codes_disabled"}));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.announcer.announced().is_empty());
}

#[tokio::test]
async fn a_pond_that_cannot_make_a_sound_answers_503_and_issues_no_code() {
    for wiring in ["no announcer", "announcer reports no speaker"] {
        let h = harness_with(|state| {
            if wiring == "no announcer" {
                state.spoken_codes = Arc::new(SpokenCodes::default());
            }
        })
        .await;
        if wiring != "no announcer" {
            h.announcer.set_unavailable(true);
        }
        let answer = post(&h.loopback, "/api/v1/handshake/spoken-code", None).await;
        assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE, "{wiring}");
        assert_eq!(answer.json, json!({"error": "announcer_unavailable"}));
        let pending = post(
            &h.loopback,
            "/api/v1/handshake/pake/init",
            Some(init_body("phone", &[1u8; 32], None)),
        )
        .await;
        assert_eq!(
            pending.json["error"], "no_active_pairing_code",
            "{wiring}: a code nobody can hear must not have been issued"
        );
    }
}

#[tokio::test]
async fn the_setup_and_spoken_routes_are_refused_off_the_direct_lan() {
    let h = harness().await;
    let spoken = post(&h.tailnet, "/api/v1/handshake/spoken-code", None).await;
    assert_eq!(spoken.status, StatusCode::FORBIDDEN);
    assert_eq!(spoken.json, json!({"error": "pairing_requires_lan"}));
    let setup = post(
        &h.tailnet,
        "/api/v1/handshake/pake/init",
        Some(init_body("phone", &[1u8; 32], Some(json!("setup")))),
    )
    .await;
    assert_eq!(setup.status, StatusCode::FORBIDDEN);
    assert_eq!(setup.json, json!({"error": "pairing_requires_lan"}));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.announcer.announced().is_empty());
}
