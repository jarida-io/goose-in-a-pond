//! Developer mode over HTTP: only a Pond the host marked as a test Pond lets a phone turn the
//! debug switch on, and taking the mark away turns it off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_api::developer::DeveloperMode;
use pond_core::security::domain::developer_mode::DebugSwitch;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::security::ports::test_pond::TestPondMark;
use serde_json::{json, Value};
use tower::ServiceExt;

const PHONE: &str = "phone-token";

struct OnePhone;

#[async_trait]
impl Handshake for OnePhone {
    async fn revoke_device(&self, _: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used here")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(token == PHONE)
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        Ok((token == PHONE).then(|| TokenCaller {
            client_id: "phone".into(),
            device_id: "phone".into(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used here")
    }
}

#[derive(Default)]
struct Mark(AtomicBool);

#[async_trait]
impl TestPondMark for Mark {
    async fn is_test_pond(&self) -> Result<bool> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

struct Pond {
    router: axum::Router,
    mark: Arc<Mark>,
    debug: DebugSwitch,
    _dir: tempfile::TempDir,
}

/// `marked: None` is a Pond from before developer mode's mark existed: no mark at all.
async fn pond(marked: Option<bool>) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    state.handshake = Arc::new(OnePhone);
    let mark = Arc::new(Mark(AtomicBool::new(marked.unwrap_or(false))));
    let debug = DebugSwitch::default();
    if marked.is_some() {
        state.developer = Arc::new(DeveloperMode::new(mark.clone(), debug.clone()));
    }
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        mark,
        debug,
        _dir: built.dir,
    }
}

async fn send(
    pond: &Pond,
    method: Method,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri("/api/v1/developer/debug");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = pond
        .router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn developer_mode_needs_a_paired_device() {
    let pond = pond(Some(true)).await;
    let (status, _) = send(&pond, Method::GET, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&pond, Method::PUT, None, Some(json!({ "enabled": true }))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!pond.debug.is_on());
}

#[tokio::test]
async fn a_home_pond_says_so_and_refuses_the_switch() {
    for marked in [None, Some(false)] {
        let pond = pond(marked).await;
        let (status, body) = send(&pond, Method::GET, Some(PHONE), None).await;
        assert_eq!(
            (status, body),
            (
                StatusCode::OK,
                json!({ "test_pond": false, "debug": false })
            )
        );

        let (status, body) = send(
            &pond,
            Method::PUT,
            Some(PHONE),
            Some(json!({ "enabled": true })),
        )
        .await;
        assert_eq!(
            (status, body),
            (StatusCode::CONFLICT, json!({ "error": "not_a_test_pond" }))
        );
        assert!(!pond.debug.is_on());
    }
}

#[tokio::test]
async fn a_test_pond_turns_debug_on_and_off() {
    let pond = pond(Some(true)).await;
    let (status, body) = send(&pond, Method::GET, Some(PHONE), None).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({ "test_pond": true, "debug": false }))
    );

    let (status, body) = send(
        &pond,
        Method::PUT,
        Some(PHONE),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({ "test_pond": true, "debug": true }))
    );
    assert!(pond.debug.is_on());

    let (status, body) = send(
        &pond,
        Method::PUT,
        Some(PHONE),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, json!({ "test_pond": true, "debug": false }))
    );
    assert!(!pond.debug.is_on());
}

#[tokio::test]
async fn taking_the_mark_away_turns_debug_off() {
    let pond = pond(Some(true)).await;
    send(
        &pond,
        Method::PUT,
        Some(PHONE),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert!(pond.debug.is_on());

    pond.mark.0.store(false, Ordering::SeqCst);
    let (_, body) = send(&pond, Method::GET, Some(PHONE), None).await;
    assert_eq!(body, json!({ "test_pond": false, "debug": false }));
    assert!(!pond.debug.is_on());
}

#[tokio::test]
async fn a_body_without_enabled_changes_nothing() {
    let pond = pond(Some(true)).await;
    let (status, _) = send(&pond, Method::PUT, Some(PHONE), Some(json!({ "on": true }))).await;
    assert!(status.is_client_error(), "{status}");
    assert!(!pond.debug.is_on());
}
