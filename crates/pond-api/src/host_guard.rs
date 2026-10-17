//! The loopback dashboard answers only requests addressed to a loopback name, from a
//! first-party page.
//!
//! Binding to `127.0.0.1` keeps other machines out, but not a web page in the host's own
//! browser. A page on `http://attacker.example` can re-point its name at `127.0.0.1` (DNS
//! rebinding) and then read and write this API as a same-origin page; any page can also send
//! a body-less cross-site `POST`. The first carries the attacker's name in `Host`, the second
//! the attacker's origin in `Origin`, and neither can forge a loopback value for them.
//! A page on another loopback port is not first party either: it may be another account's
//! server, or anything else listening on this machine, so `Origin` must name the very host and
//! port the request was sent to.
//!
//! A pairing code is a full device token, so minting one needs more than reaching loopback:
//! the caller must also present the [`HostCredential`], which the server writes to a `0600`
//! file in its data directory at every start. The desktop app reads that file; a browser gets
//! it from the sign-in link `pond-server dashboard` prints; other OS accounts cannot read it.

use axum::{
    extract::Request,
    http::{header, uri::Authority, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// Request header carrying the [`HostCredential`].
pub const CREDENTIAL_HEADER: &str = "x-pond-host-credential";

/// Proof that the caller can read this Pond's data directory. Rotated at every start.
#[derive(Clone)]
pub struct HostCredential(Arc<str>);

impl HostCredential {
    /// 32 random bytes, base64url without padding: 43 characters, safe in a URL fragment.
    pub fn generate() -> Self {
        use base64::Engine;
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(bytes)
                .into(),
        )
    }

    /// The value to write to the credential file and to send in [`CREDENTIAL_HEADER`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Admit the request only with the current credential, unless `POND_DEV_ALLOW_LOOPBACK` is
/// set for local development. `None` means the server was wired without one: refuse.
pub fn require_credential(
    expected: Option<&HostCredential>,
    headers: &HeaderMap,
    operation: &'static str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let Some(expected) = expected else {
        tracing::warn!(
            operation,
            kind = "host_credential_unavailable",
            "this listener holds no host credential; refusing"
        );
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "host_credential_unavailable"})),
        ));
    };
    if presents_credential(Some(expected), headers) {
        return Ok(());
    }
    let presented = headers
        .get(CREDENTIAL_HEADER)
        .is_some_and(|value| !value.is_empty());
    tracing::warn!(
        operation,
        presented,
        kind = "host_credential_rejected",
        "refused a host-only request without the current host credential"
    );
    Err((
        StatusCode::FORBIDDEN,
        Json(json!({"error": "host_credential_required"})),
    ))
}

/// Whether the request holds the current credential, without logging a refusal: for routes where
/// the host is one of several callers allowed, and the others are judged next.
pub fn presents_credential(expected: Option<&HostCredential>, headers: &HeaderMap) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let presented = headers
        .get(CREDENTIAL_HEADER)
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    let matches: bool = presented.ct_eq(expected.as_str().as_bytes()).into();
    matches || (presented.is_empty() && crate::middleware::dev_allow_loopback())
}

/// Names that only ever resolve to this machine. The port is not checked, so an SSH tunnel
/// (`ssh -L 9000:localhost:4000`) keeps working.
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Refuse a request whose `Host` is not a loopback name, or whose `Origin` is not first party.
pub async fn loopback_only(request: Request, next: Next) -> Response {
    let authority = request
        .uri()
        .authority()
        .cloned()
        .or_else(|| host_authority(request.headers()));
    let host = authority.as_ref().map(|authority| authority.host());
    let Some(authority) = authority
        .as_ref()
        .filter(|authority| is_loopback_host(authority.host()))
    else {
        tracing::warn!(
            kind = "loopback_host_rejected",
            host = %bounded(host.unwrap_or("")),
            path = %request.uri().path(),
            "refused a dashboard request not addressed to a loopback name"
        );
        return (
            StatusCode::MISDIRECTED_REQUEST,
            Json(json!({"error": "loopback_host_required"})),
        )
            .into_response();
    };
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !is_first_party_origin(origin, authority) {
            tracing::warn!(
                kind = "cross_origin_rejected",
                origin = %bounded(origin.to_str().unwrap_or("<not text>")),
                path = %request.uri().path(),
                "refused a dashboard request from a page that is not the Pond's own"
            );
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "origin_not_allowed"})),
            )
                .into_response();
        }
    }
    next.run(request).await
}

pub(crate) fn host_header(headers: &HeaderMap) -> Option<String> {
    host_authority(headers).map(|authority| authority.host().to_owned())
}

/// The `Host` header as an authority: the host and the port the client sent the request to.
fn host_authority(headers: &HeaderMap) -> Option<Authority> {
    headers.get(header::HOST)?.to_str().ok()?.parse().ok()
}

fn is_loopback_host(host: &str) -> bool {
    LOOPBACK_HOSTS
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
}

/// One of the CORS origins, or a page served by this very listener: an `http://` origin on a
/// loopback name whose host and port are the ones the request was sent to.
fn is_first_party_origin(origin: &HeaderValue, host: &Authority) -> bool {
    if crate::allowed_origins()
        .iter()
        .any(|allowed| allowed == origin)
    {
        return true;
    }
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some(rest) = origin
        .strip_prefix("http://")
        .filter(|rest| !rest.contains('@'))
    else {
        return false;
    };
    rest.parse::<Authority>().is_ok_and(|authority| {
        authority.as_str() == rest
            && is_loopback_host(authority.host())
            && same_authority(&authority, host)
    })
}

/// Host compared without case, port defaulting to 80 on both sides: the dashboard is plain HTTP.
fn same_authority(origin: &Authority, host: &Authority) -> bool {
    origin.host().eq_ignore_ascii_case(host.host())
        && origin.port_u16().unwrap_or(80) == host.port_u16().unwrap_or(80)
}

/// Attacker-supplied text goes to the log, so keep it short.
pub(crate) fn bounded(text: &str) -> String {
    text.chars().take(100).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    async fn status(host: Option<&str>, origin: Option<&str>) -> StatusCode {
        let app = Router::new()
            .route("/api/v1/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(loopback_only));
        let mut request = Request::get("/api/v1/health");
        if let Some(host) = host {
            request = request.header(header::HOST, host);
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        app.oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[test]
    fn only_the_current_credential_is_accepted() {
        let credential = HostCredential::generate();
        assert_eq!(credential.as_str().len(), 43);
        assert_ne!(credential.as_str(), HostCredential::generate().as_str());
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(CREDENTIAL_HEADER, value.parse().unwrap());
            headers
        };
        assert!(require_credential(Some(&credential), &with(credential.as_str()), "t").is_ok());
        let stale = HostCredential::generate();
        for headers in [with(stale.as_str()), with("x"), HeaderMap::new()] {
            let (status, body) = require_credential(Some(&credential), &headers, "t").unwrap_err();
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(body.0["error"], "host_credential_required");
        }
        let (status, _) = require_credential(None, &with(credential.as_str()), "t").unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn loopback_names_on_any_port_are_served() {
        for host in [
            "127.0.0.1:4000",
            "localhost:4000",
            "LOCALHOST:9000",
            "[::1]:4001",
            "127.0.0.1",
        ] {
            assert_eq!(status(Some(host), None).await, StatusCode::OK, "{host}");
        }
    }

    #[tokio::test]
    async fn a_rebound_or_missing_host_is_refused() {
        for host in [
            Some("attacker.example:4000"),
            Some("127.0.0.1.attacker.example"),
            Some("localhost.attacker.example:4000"),
            Some("192.168.1.2:4000"),
            Some("0.0.0.0:4000"),
            None,
        ] {
            assert_eq!(
                status(host, None).await,
                StatusCode::MISDIRECTED_REQUEST,
                "{host:?}"
            );
        }
    }

    #[tokio::test]
    async fn only_first_party_pages_may_send_requests() {
        for (host, origin) in [
            ("127.0.0.1:4000", "http://127.0.0.1:4000"),
            ("LOCALHOST:9000", "http://localhost:9000"),
            ("[::1]:4000", "http://[::1]:4000"),
            // No port on either side is port 80 on both.
            ("127.0.0.1", "http://127.0.0.1"),
            ("127.0.0.1:80", "http://127.0.0.1"),
            // An SSH tunnel's page talks to the tunnel's own port.
            ("localhost:9000", "http://localhost:9000"),
            // The packaged shell and, in a debug build, the Vite dev server: the CORS allowlist.
            ("127.0.0.1:4000", "app://giap"),
            ("127.0.0.1:4000", "http://localhost:1420"),
        ] {
            assert_eq!(
                status(Some(host), Some(origin)).await,
                StatusCode::OK,
                "{origin} to {host}"
            );
        }
        for (host, origin) in [
            ("127.0.0.1:4000", "http://attacker.example"),
            ("127.0.0.1:4000", "https://127.0.0.1:4000"),
            ("127.0.0.1:4000", "http://localhost.attacker.example"),
            ("127.0.0.1:4000", "http://127.0.0.1:4000/path"),
            ("localhost:4000", "http://user@localhost:4000"),
            ("127.0.0.1:4000", "null"),
            // A page on another loopback port: another account's server, or any local listener.
            ("127.0.0.1:4000", "http://127.0.0.1:8080"),
            ("127.0.0.1:4000", "http://localhost:9000"),
            // The same port under another loopback name is another origin.
            ("127.0.0.1:4000", "http://localhost:4000"),
            ("127.0.0.1:4000", "http://[::1]:4000"),
            ("127.0.0.1:4000", "http://127.0.0.1"),
            ("127.0.0.1", "http://127.0.0.1:4000"),
        ] {
            assert_eq!(
                status(Some(host), Some(origin)).await,
                StatusCode::FORBIDDEN,
                "{origin} to {host}"
            );
        }
    }
}
