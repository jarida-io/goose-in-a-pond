//! The pairing lock: once the pond has a main member with a phone, another phone that proves a
//! pairing code is not let in. It leaves a request the main member accepts or declines, and a
//! recovery PIN lets the main member's own new phone in.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(
    router: &axum::Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
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

async fn claim(router: &axum::Router, held: &Value) -> (StatusCode, Value) {
    call(
        router,
        Method::POST,
        "/api/v1/handshake/pairing-request/claim",
        None,
        Some(json!({
            "request_id": held["request_id"],
            "claim_secret": held["claim_secret"],
        })),
    )
    .await
}

#[tokio::test]
async fn a_locked_pond_holds_a_phone_until_its_main_member_lets_it_in() {
    let pond_api::test_support::TestState {
        state,
        handshake,
        dir: _dir,
    } = pond_api::test_support::app_state().await;
    let jerry = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "duck".into(),
        })
        .await
        .unwrap()
        .id;
    let state: Arc<AppState> = Arc::new(state);
    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));

    // Nobody holds the main member's seat and no phone is paired, so nobody could answer: his
    // own phone pairs as before, and makes him the main member.
    let code = handshake
        .issue_pairing_code_for(Some(&jerry))
        .await
        .unwrap()
        .code;
    let (status, his) = pair_phone(&router, &code, "jerry-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{his}");
    assert_eq!(his["accepted"], true, "{his}");
    let his_token = his["session_token"].as_str().unwrap().to_string();
    let (_, lock) = call(
        &router,
        Method::GET,
        "/api/v1/pairing/lock",
        Some(&his_token),
        None,
    )
    .await;
    assert_eq!(lock["owner_profile_id"], jerry.as_str(), "{lock}");
    assert_eq!(lock["enforced"], true, "{lock}");
    assert_eq!(lock["can_change"], true, "{lock}");

    // Now a second phone proves a code and is held.
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (status, guest) = pair_phone(&router, &code, "guest-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{guest}");
    assert_eq!(guest["accepted"], false, "{guest}");
    assert_eq!(guest["error"], "approval_required", "{guest}");
    assert!(guest["session_token"].is_null(), "{guest}");
    assert!(
        guest["server_proof"].is_string(),
        "the pond still proves itself"
    );
    let held = guest["pairing_request"].clone();
    assert_eq!(claim(&router, &held).await.0, StatusCode::ACCEPTED);
    let wrong = json!({"request_id": held["request_id"], "claim_secret": "not-the-secret"});
    assert_eq!(claim(&router, &wrong).await.0, StatusCode::GONE);

    let (status, waiting) = call(
        &router,
        Method::GET,
        "/api/v1/pairing/requests",
        Some(&his_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{waiting}");
    assert_eq!(
        waiting["requests"].as_array().unwrap().len(),
        1,
        "{waiting}"
    );
    assert_eq!(
        waiting["requests"][0]["device_name"], "guest-phone",
        "{waiting}"
    );
    assert!(waiting["requests"][0].get("claim_secret").is_none());

    let request_id = held["request_id"].as_str().unwrap();
    let approve = format!("/api/v1/pairing/requests/{request_id}/approve");
    let (status, body) = call(&router, Method::POST, &approve, Some(&his_token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, paired) = claim(&router, &held).await;
    assert_eq!(status, StatusCode::OK, "{paired}");
    assert_eq!(paired["status"], "paired");
    let guest_token = paired["session_token"].as_str().unwrap().to_string();
    let (status, me) = call(
        &router,
        Method::GET,
        "/api/v1/devices/self",
        Some(&guest_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(
        claim(&router, &held).await.0,
        StatusCode::GONE,
        "collected once"
    );

    // A phone that is not the main member's cannot answer or change anything.
    let (status, _) = call(
        &router,
        Method::GET,
        "/api/v1/pairing/requests",
        Some(&guest_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &router,
        Method::PUT,
        "/api/v1/pairing/lock",
        Some(&guest_token),
        Some(json!({"locked": false})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A declined phone is told so.
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (_, stranger) = pair_phone(&router, &code, "stranger-phone", TEST_SPKI_PIN).await;
    let held = stranger["pairing_request"].clone();
    let decline = format!(
        "/api/v1/pairing/requests/{}/decline",
        held["request_id"].as_str().unwrap()
    );
    let (status, _) = call(&router, Method::POST, &decline, Some(&his_token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(claim(&router, &held).await.0, StatusCode::FORBIDDEN);

    // The recovery PIN lets the main member's new phone in as him.
    let (status, _) = call(
        &router,
        Method::PUT,
        "/api/v1/pairing/recovery-pin",
        Some(&his_token),
        Some(json!({"pin": "12345"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "five digits is too short");
    let (status, _) = call(
        &router,
        Method::PUT,
        "/api/v1/pairing/recovery-pin",
        Some(&his_token),
        Some(json!({"pin": "246810"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (_, new_phone) = pair_phone(&router, &code, "jerry-new-phone", TEST_SPKI_PIN).await;
    let held = new_phone["pairing_request"].clone();
    let recover = |pin: &'static str| {
        let router = &router;
        let held = held.clone();
        async move {
            call(
                router,
                Method::POST,
                "/api/v1/handshake/pairing-request/recover",
                None,
                Some(json!({
                    "request_id": held["request_id"],
                    "claim_secret": held["claim_secret"],
                    "pin": pin,
                })),
            )
            .await
        }
    };
    let (status, wrong) = recover("000000").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{wrong}");
    assert_eq!(wrong["tries_left"], 4, "{wrong}");
    let (status, recovered) = recover("246810").await;
    assert_eq!(status, StatusCode::OK, "{recovered}");
    let new_token = recovered["session_token"].as_str().unwrap().to_string();
    let (_, lock) = call(
        &router,
        Method::GET,
        "/api/v1/pairing/lock",
        Some(&new_token),
        None,
    )
    .await;
    assert_eq!(
        lock["can_change"], true,
        "the PIN let him in as the main member: {lock}"
    );

    // A code whoever holds the pond already accepted needs no request.
    let code = handshake.issue_pairing_code().await.unwrap().code;
    handshake.mark_pairing_code_approved(&code).await.unwrap();
    let (_, accepted) = pair_phone(&router, &code, "approved-phone", TEST_SPKI_PIN).await;
    assert_eq!(accepted["accepted"], true, "{accepted}");

    // And with the lock off, pairing is as it was.
    let (status, _) = call(
        &router,
        Method::PUT,
        "/api/v1/pairing/lock",
        Some(&his_token),
        Some(json!({"locked": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (_, open) = pair_phone(&router, &code, "open-phone", TEST_SPKI_PIN).await;
    assert_eq!(open["accepted"], true, "{open}");
}
