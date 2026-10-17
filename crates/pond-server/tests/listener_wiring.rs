//! Which routes each listener serves. `serve()` binds exactly what `compose()` returns, so
//! these checks cover the production wiring without starting the server.

use std::net::SocketAddr;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use pond_api::network::CompanionTransport;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, PairingCode, PakeChallenge, PakeInitRequest,
    PakeRefusal, PakeVerifyRequest, TokenCaller,
};
use pond_server::listeners::{compose, Listeners};
use serde_json::Value;
use tower::ServiceExt;

const LOOPBACK: ([u8; 4], u16) = ([127, 0, 0, 1], 40_000);
const LAN: ([u8; 4], u16) = ([192, 168, 1, 20], 40_000);

/// Accepts one bearer, so a 404 means the route is absent rather than the caller refused.
/// Pairing codes and CPace pairing come from the real adapter.
struct PairedPhone(std::sync::Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>);

#[async_trait]
impl Handshake for PairedPhone {
    async fn revoke_device(&self, _: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not exercised by the wiring tests")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(token == TOKEN)
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        Ok((token == TOKEN).then(|| TokenCaller {
            client_id: "phone".into(),
            device_id: "phone".into(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn issue_pairing_code_for(&self, profile: Option<&str>) -> Result<PairingCode> {
        self.0.issue_pairing_code_for(profile).await
    }
    async fn init_pake(
        &self,
        request: PakeInitRequest,
    ) -> Result<std::result::Result<PakeChallenge, PakeRefusal>> {
        self.0.init_pake(request).await
    }
    async fn verify_pake(&self, request: PakeVerifyRequest) -> Result<HandshakeResponse> {
        self.0.verify_pake(request).await
    }
}

const TOKEN: &str = "wiring-test-token";

struct Harness {
    listeners: Listeners,
    credential: pond_api::host_guard::HostCredential,
    #[cfg(unix)]
    presence: std::sync::Arc<dyn pond_core::security::ports::remote_access::DevicePresence>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    harness_with(None).await
}

/// The same, with the plaintext development listener on or off.
async fn harness_with(insecure: Option<pond_api::insecure_dev::InsecureDevLan>) -> Harness {
    let test = pond_api::test_support::app_state().await;
    let transport = CompanionTransport {
        https_port: 4443,
        // The adapter's pin, as `serve()` hands both the same one.
        tls_spki_sha256: pond_api::test_support::TEST_SPKI_PIN.into(),
    };
    #[cfg(unix)]
    let (embedded, _socket) =
        pond_server::embedded_network::Runtime::new(test.dir.path(), 4000).unwrap();
    let credential = pond_api::host_guard::HostCredential::generate();
    #[cfg(unix)]
    let presence = embedded.clone();
    // Serves `/` when only the build.rs placeholder is embedded, as on a fresh checkout.
    let web = test.dir.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html>").unwrap();
    let listeners = compose(
        std::sync::Arc::new(pond_api::AppState {
            handshake: std::sync::Arc::new(PairedPhone(test.handshake)),
            ..test.state
        }),
        web,
        transport,
        credential.clone(),
        insecure,
        #[cfg(unix)]
        embedded,
    );
    Harness {
        listeners,
        credential,
        #[cfg(unix)]
        presence,
        _dir: test.dir,
    }
}

/// What a browser or phone puts in `Host` when it reaches that listener.
fn host_for(peer: ([u8; 4], u16)) -> &'static str {
    if peer == LOOPBACK {
        "127.0.0.1:4000"
    } else {
        "pond.local:4443"
    }
}

async fn get(router: &axum::Router, peer: ([u8; 4], u16), path: &str) -> (StatusCode, Value) {
    send(
        // The real extension, as `into_make_service_with_connect_info` inserts it: middleware
        // that reads it directly never sees `MockConnectInfo`.
        router
            .clone()
            .layer(axum::Extension(ConnectInfo(SocketAddr::from(peer)))),
        Request::get(path)
            .header("Host", host_for(peer))
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn send(router: axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn remote_access_management_is_served_only_by_the_dashboard() {
    let h = harness().await;
    let (dashboard, _) = get(&h.listeners.dashboard, LOOPBACK, "/api/v1/remote-access").await;
    assert_ne!(
        dashboard,
        StatusCode::NOT_FOUND,
        "the dashboard lost management"
    );
    let (companion, _) = get(&h.listeners.companion, LAN, "/api/v1/remote-access").await;
    assert_eq!(companion, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_companion_serves_no_dashboard_assets() {
    let h = harness().await;
    let (status, _) = get(&h.listeners.companion, LAN, "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&h.listeners.dashboard, LOOPBACK, "/").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn both_public_listeners_advertise_the_https_transport() {
    let h = harness().await;
    for (router, peer) in [
        (&h.listeners.dashboard, LOOPBACK),
        (&h.listeners.companion, LAN),
    ] {
        let (status, body) = get(router, peer, "/api/v1/system/info").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["https_port"], 4443);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_embedded_socket_admits_only_forwarded_tailnet_peers() {
    let h = harness().await;
    let request = |peer: Option<&str>| {
        let mut request =
            Request::get("/api/v1/health").header("Authorization", format!("Bearer {TOKEN}"));
        if let Some(peer) = peer {
            request = request.header("x-pond-embedded-peer", peer);
        }
        request.body(Body::empty()).unwrap()
    };
    let (status, _) = send(h.listeners.embedded.clone(), request(None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no forwarded peer");
    let (status, _) = send(
        h.listeners.embedded.clone(),
        request(Some("127.0.0.1:1234")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a forwarded peer is never loopback"
    );
    let (status, _) = send(
        h.listeners.embedded.clone(),
        request(Some("100.64.0.9:1234")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        h.listeners.embedded.clone(),
        Request::get("/api/v1/remote-access")
            .header("x-pond-embedded-peer", "100.64.0.9:1234")
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "management reached the tailnet"
    );
}

#[tokio::test]
async fn the_dashboard_refuses_rebound_and_cross_site_requests() {
    let h = harness().await;
    let dashboard = || {
        h.listeners
            .dashboard
            .clone()
            .layer(axum::Extension(ConnectInfo(SocketAddr::from(LOOPBACK))))
    };
    for path in [
        "/",
        "/api/v1/remote-access",
        "/api/v1/handshake/pairing-code",
    ] {
        let (status, body) = send(
            dashboard(),
            Request::get(path)
                .header("Host", "attacker.example:4000")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{path}");
        assert_eq!(body["error"], "loopback_host_required", "{path}");
        let (status, body) = send(
            dashboard(),
            Request::post(path)
                .header("Host", "127.0.0.1:4000")
                .header("Origin", "http://attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "origin_not_allowed", "{path}");
        // A page on another loopback port is another origin: any account's server, or any
        // other local listener, can serve one.
        let (status, body) = send(
            dashboard(),
            Request::post(path)
                .header("Host", "127.0.0.1:4000")
                .header("Origin", "http://127.0.0.1:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "origin_not_allowed", "{path}");
    }
}

#[tokio::test]
async fn pairing_codes_need_the_host_credential_even_on_loopback() {
    let h = harness().await;
    let issue = |credential: Option<&str>| {
        let mut request = Request::post("/api/v1/handshake/pairing-code")
            .header("Host", "127.0.0.1:4000")
            .header("Origin", "http://127.0.0.1:4000");
        if let Some(credential) = credential {
            request = request.header(pond_api::host_guard::CREDENTIAL_HEADER, credential);
        }
        send(
            h.listeners
                .dashboard
                .clone()
                .layer(axum::Extension(ConnectInfo(SocketAddr::from(LOOPBACK)))),
            request.body(Body::empty()).unwrap(),
        )
    };
    let (status, body) = issue(None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "host_credential_required");
    let (status, body) = issue(Some(h.credential.as_str())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["code"].is_string(), "{body}");
    // The pairing material a phone needs travels with the code, not through /system/info.
    assert_eq!(body["pairing"]["https_port"], 4443);
    assert!(body["pairing"]["tls_spki_sha256"]
        .as_str()
        .is_some_and(|pin| pin.starts_with("sha256/")));
    assert!(body["pairing"]["hostname"].is_string());
}

/// Only the dashboard on the host learns the software version from `/health`; a LAN or
/// tailnet peer learns that the Pond is up and which release to aim at otherwise.
#[tokio::test]
async fn health_reports_the_version_only_to_the_host() {
    let h = harness().await;
    let (status, body) = get(&h.listeners.dashboard, LOOPBACK, "/api/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["version"].is_string(), "{body}");

    let (status, body) = get(&h.listeners.companion, LAN, "/api/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "ok"}));

    #[cfg(unix)]
    {
        let (status, body) = send(
            h.listeners.embedded.clone(),
            Request::get("/api/v1/health")
                .header("x-pond-embedded-peer", "100.64.0.9:1234")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"status": "ok"}));
    }
}

/// `POST` to `path` from a loopback peer, as the dashboard page sends it, with the host
/// credential and a bearer when given.
async fn post_from_the_host(
    router: &axum::Router,
    path: &str,
    credential: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::post(path)
        .header("Host", "127.0.0.1:4000")
        .header("Origin", "http://127.0.0.1:4000")
        .header("Content-Type", "application/json");
    if let Some(credential) = credential {
        request = request.header(pond_api::host_guard::CREDENTIAL_HEADER, credential);
    }
    if let Some(bearer) = bearer {
        request = request.header("Authorization", format!("Bearer {bearer}"));
    }
    send(
        router
            .clone()
            .layer(axum::Extension(ConnectInfo(SocketAddr::from(LOOPBACK)))),
        request.body(Body::from(r#"{"text":"hello"}"#)).unwrap(),
    )
    .await
}

/// Resetting onboarding reopens the wizard's writes to any tokenless process on an unclaimed host,
/// and speaking uses the host's speaker. Being a loopback peer proves neither is the Pond's own.
#[tokio::test]
async fn onboarding_reset_and_test_speech_need_the_host_credential_on_loopback() {
    let h = harness().await;
    for path in ["/api/v1/onboard/reset", "/api/v1/test/speak"] {
        let (status, body) = post_from_the_host(&h.listeners.dashboard, path, None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "host_credential_required", "{path}");
        let (status, _) = post_from_the_host(
            &h.listeners.dashboard,
            path,
            Some(h.credential.as_str()),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");

        // The companion holds no credential to compare against, so a loopback peer there is
        // refused rather than let through.
        let (status, body) = post_from_the_host(&h.listeners.companion, path, None, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(body["error"], "host_credential_unavailable", "{path}");
    }
    // A paired device's token still restarts setup, even as a loopback peer of the companion.
    let (status, _) = post_from_the_host(
        &h.listeners.companion,
        "/api/v1/onboard/reset",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn system_info_tells_an_anonymous_caller_where_to_connect_and_nothing_more() {
    let h = harness().await;
    let (status, body) = send(
        h.listeners.companion.clone(),
        Request::get("/api/v1/system/info")
            .header("Host", "pond.local:4443")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["https_port"], 4443);
    assert_eq!(body["protocol"], 3);
    for private in [
        "hostname",
        "lan_address",
        "tls_spki_sha256",
        "version",
        "platform",
        "arch",
    ] {
        assert!(
            body.get(private).is_none(),
            "{private} reached an anonymous caller: {body}"
        );
    }
    let (status, body) = get(&h.listeners.companion, LAN, "/api/v1/system/info").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["hostname"].is_string() && body["version"].is_string(),
        "{body}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn only_an_https_request_from_home_renews_remote_access() {
    // An authenticated route: public ones skip the middleware that records a sighting.
    let tailnet = harness().await;
    let (status, _) = send(
        tailnet.listeners.embedded.clone(),
        Request::get("/api/v1/devices")
            .header("x-pond-embedded-peer", "100.64.0.9:1234")
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tailnet.presence.lapses_at("phone").await.unwrap().is_none(),
        "a tailnet request counted as being at home"
    );

    let dashboard = harness().await;
    let (status, _) = get(&dashboard.listeners.dashboard, LOOPBACK, "/api/v1/devices").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        dashboard
            .presence
            .lapses_at("phone")
            .await
            .unwrap()
            .is_none(),
        "the desktop dashboard recorded a phone sighting"
    );

    // A directly attached peer; loopback stands in for the LAN, which this host may not have.
    let companion = harness().await;
    let (status, _) = get(&companion.listeners.companion, LOOPBACK, "/api/v1/devices").await;
    assert_eq!(status, StatusCode::OK);
    assert!(companion
        .presence
        .lapses_at("phone")
        .await
        .unwrap()
        .is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn the_plaintext_listener_never_enrolls_or_renews_remote_access() {
    let h = harness_with(Some(pond_api::insecure_dev::InsecureDevLan { port: 4080 })).await;
    // Loopback stands in for the LAN, as above: over HTTPS this request would renew.
    let (status, _) = get(&h.listeners.insecure, LOOPBACK, "/api/v1/devices").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        h.presence.lapses_at("phone").await.unwrap().is_none(),
        "a plaintext request counted as being at home"
    );
    // Enrollment is served over HTTPS, never over plaintext.
    let path = "/api/v1/remote-access/configuration";
    let (https, _) = get(&h.listeners.companion, LOOPBACK, path).await;
    assert_ne!(
        https,
        StatusCode::NOT_FOUND,
        "the companion lost enrollment"
    );
    let (plaintext, _) = get(&h.listeners.insecure, LOOPBACK, path).await;
    assert_eq!(plaintext, StatusCode::NOT_FOUND);
}

/// Expo Go pairs over the plaintext listener with CPace, under the pin that listener's
/// `/system/info` reports, exactly as the app does in development.
#[tokio::test]
async fn a_phone_pairs_with_cpace_over_the_plaintext_listener() {
    let h = harness_with(Some(pond_api::insecure_dev::InsecureDevLan { port: 4080 })).await;
    let (status, info) = send(
        h.listeners.insecure.clone(),
        Request::get("/api/v1/system/info")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["protocol"], 3);
    assert_eq!(info["insecure_dev"], true);
    let pin = info["tls_spki_sha256"]
        .as_str()
        .expect("the pin is reported here");

    let (status, issued) = send(
        h.listeners
            .dashboard
            .clone()
            .layer(axum::Extension(ConnectInfo(SocketAddr::from(LOOPBACK)))),
        Request::post("/api/v1/handshake/pairing-code")
            .header("Host", "127.0.0.1:4000")
            .header("Origin", "http://127.0.0.1:4000")
            .header(
                pond_api::host_guard::CREDENTIAL_HEADER,
                h.credential.as_str(),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    let code = issued["code"].as_str().unwrap();

    let phone = h
        .listeners
        .insecure
        .clone()
        .layer(axum::Extension(ConnectInfo(SocketAddr::from(LOOPBACK))));
    let (status, body) = pond_api::test_support::pair_phone(&phone, code, "expo-go", pin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true, "{body}");
}
