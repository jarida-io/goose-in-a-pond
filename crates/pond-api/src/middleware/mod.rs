//! Authentication, rate limiting, onboarding, and debug-logging middleware.

pub mod onboarding_guard;

use axum::{
    extract::{Request, State},
    http::{header, header::HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

#[derive(Debug)]
pub enum AuthError {
    MissingToken,
    InvalidFormat,
    InvalidToken,
    /// Carries how long the client should wait, for `Retry-After`.
    RateLimitExceeded {
        retry_after_secs: u64,
    },
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing Authorization header"),
            AuthError::InvalidFormat => (
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization header format. Use: Authorization: Bearer <token>",
            ),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid or expired token"),
            AuthError::RateLimitExceeded { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded")
            }
        };
        let body =
            serde_json::json!({ "error": error_message, "status": status.as_u16() }).to_string();
        match self {
            // RFC 9110: a 429 SHOULD tell the client how long to wait.
            AuthError::RateLimitExceeded { retry_after_secs } => (
                status,
                [(header::RETRY_AFTER, retry_after_secs.to_string())],
                body,
            )
                .into_response(),
            _ => (status, body).into_response(),
        }
    }
}

pub fn extract_bearer_token(headers: &HeaderMap) -> Result<String, AuthError> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(AuthError::MissingToken)?;

    if !auth_header.starts_with("Bearer ") {
        return Err(AuthError::InvalidFormat);
    }
    Ok(auth_header[7..].to_string())
}

/// Per-client fixed-window limiter: a burst straddling two windows can reach 2x `max_requests`.
pub struct RateLimiter {
    clients: Arc<RwLock<HashMap<String, ClientRateLimit>>>,
    max_requests: usize,
    window_duration: Duration,
    last_cleanup: Arc<RwLock<Instant>>,
}

#[derive(Clone)]
struct ClientRateLimit {
    window_start: Instant,
    request_count: usize,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window_duration: Duration) -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            max_requests,
            window_duration,
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    pub async fn check_rate_limit(&self, client_id: &str) -> bool {
        self.check_rate_limit_detailed(client_id).await.is_ok()
    }

    /// As [`Self::check_rate_limit`], but a rejection says how long to wait (for `Retry-After`).
    pub async fn check_rate_limit_detailed(&self, client_id: &str) -> Result<(), Duration> {
        let mut clients = self.clients.write().await;
        let now = Instant::now();
        let client = clients
            .entry(client_id.to_string())
            .or_insert(ClientRateLimit {
                window_start: now,
                request_count: 0,
            });
        if now.duration_since(client.window_start) > self.window_duration {
            client.window_start = now;
            client.request_count = 0;
        }
        let allowed = if client.request_count < self.max_requests {
            client.request_count += 1;
            Ok(())
        } else {
            // Whatever is left of the current window.
            Err(self
                .window_duration
                .saturating_sub(now.duration_since(client.window_start)))
        };

        // Periodic eviction of stale entries to prevent unbounded growth.
        let mut lc = self.last_cleanup.write().await;
        if now.duration_since(*lc) > self.window_duration * 2 {
            clients.retain(|_, v| now.duration_since(v.window_start) <= self.window_duration);
            *lc = now;
        }

        allowed
    }
}

/// Whole seconds, at least 1: `Retry-After: 0` would invite an immediate retry.
pub fn retry_after_secs(remaining: Duration) -> u64 {
    remaining.as_secs().max(1)
}

/// Unauthenticated-loopback escape hatch for local dev; off unless explicitly enabled.
fn dev_allow_loopback() -> bool {
    loopback_flag_enabled(std::env::var("POND_DEV_ALLOW_LOOPBACK").ok().as_deref())
}

/// Split from the env read so tests needn't mutate the shared process env.
fn loopback_flag_enabled(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("TRUE"))
}

/// When a route stops answering a caller with no bearer token.
/// Wizard writes must close with the wizard, or any LAN caller could rewrite settings later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exposure {
    /// Public in every state: pairing, health, onboarding status and diagnostics.
    Always,
    /// Public only while onboarding is incomplete. The wizard's own surface.
    UntilOnboarded,
    /// As [`Exposure::UntilOnboarded`], but still open to loopback once set up.
    /// Only `POST /onboard/reset`, for repair; re-pairing already requires being at the host.
    UntilOnboardedThenHostOnly,
}

/// Every `(method, path, exposure)` reachable with no bearer token; `{brace}` is one segment.
/// Must match the public router in `routes::api_routes` (`public_router_and_allowlist_agree`).
const PUBLIC_ROUTES: &[(Method, &str, Exposure)] = &[
    (Method::GET, "/health", Exposure::Always),
    // Pairing precedes any token; the pairing-code routes are loopback-gated in their handlers.
    (Method::POST, "/handshake", Exposure::Always),
    (Method::POST, "/handshake/init", Exposure::Always),
    (Method::POST, "/handshake/verify", Exposure::Always),
    (Method::POST, "/handshake/refresh", Exposure::Always),
    (Method::POST, "/handshake/revoke", Exposure::Always),
    (Method::GET, "/handshake/pairing-code", Exposure::Always),
    (Method::POST, "/handshake/pairing-code", Exposure::Always),
    // Onboarding: runs before any device pairs, and must not afterwards.
    (Method::POST, "/onboard", Exposure::UntilOnboarded),
    (Method::POST, "/onboard/complete", Exposure::UntilOnboarded),
    // ...except the status probe: a tokenless client must learn whether it needs the wizard.
    (Method::GET, "/onboard/status", Exposure::Always),
    (
        Method::POST,
        "/onboard/step/{name}",
        Exposure::UntilOnboarded,
    ),
    // The recovery lever; the only `UntilOnboardedThenHostOnly` entry.
    (
        Method::POST,
        "/onboard/reset",
        Exposure::UntilOnboardedThenHostOnly,
    ),
    // IANA zones: the same list on every pond, so it reveals nothing; needed before pairing too.
    (Method::GET, "/time/zones", Exposure::Always),
    // Detection geolocates via an outbound call, so it is open only until onboarding finishes.
    (Method::POST, "/location/detect", Exposure::UntilOnboarded),
    // Write-only, wizard-only. GET /settings is deliberately absent: it serialises all Settings.
    (Method::PUT, "/settings", Exposure::UntilOnboarded),
    // Warm-up banner during the wizard; afterwards `PondApiClient.get` sends the bearer token.
    (Method::GET, "/warmup", Exposure::UntilOnboarded),
    // Local Piper, no user data. `Always` because `playTtsSentence` (WebVoiceBackend.ts) and
    // `fetch_tts_bytes` (audio_cmd.rs) call it tokenless after setup; closing it mutes the pond.
    (Method::POST, "/tts", Exposure::Always),
    // Wizard voice setup; afterwards PondApiClient.ts sends the token (verified, unlike /tts).
    (Method::POST, "/voice/tts/apply", Exposure::UntilOnboarded),
    (Method::POST, "/voice/calibrate", Exposure::UntilOnboarded),
    (Method::DELETE, "/voice/calibrate", Exposure::UntilOnboarded),
    // Diagnostics, not wizard needs. Whether they should be unauthenticated at all is still open.
    (Method::POST, "/transcribe", Exposure::Always),
    (Method::GET, "/system/info", Exposure::Always),
    (Method::GET, "/test", Exposure::Always),
    (Method::POST, "/test/speak", Exposure::Always),
    (Method::GET, "/dev/goose", Exposure::Always),
    // Onboarding creates/patches; GET /profiles and DELETE /profiles/{id} are deliberately absent.
    (Method::POST, "/profiles", Exposure::UntilOnboarded),
    (Method::PATCH, "/profiles/{id}", Exposure::UntilOnboarded),
    // Protected-router routes guarded by other means: the redirect target, by its PKCE nonce...
    (Method::GET, "/oauth/callback", Exposure::Always),
    // ...and extensions' refresh, checked against internal_extension_token in the handler.
    (Method::POST, "/oauth/refresh", Exposure::Always),
];

/// Segment-wise route match; `{brace}` matches exactly one non-empty segment, never more.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pat = pattern.split('/');
    let mut act = path.split('/');
    loop {
        match (pat.next(), act.next()) {
            (None, None) => return true,
            (Some(p), Some(a)) => {
                if p.starts_with('{') && p.ends_with('}') {
                    // `/profiles/` must not match `/profiles/{id}`.
                    if a.is_empty() {
                        return false;
                    }
                } else if p != a {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// This request's exposure class, or `None` when the route is not on the allowlist.
fn route_exposure(method: &Method, path: &str) -> Option<Exposure> {
    // Non-API paths are dashboard static assets (`serve_web` only reads files).
    // Returns before any DB access: every asset request passes through here.
    if !path.starts_with("/api/") {
        return Some(Exposure::Always);
    }

    let path = path.strip_prefix("/api/v1").unwrap_or(path);

    PUBLIC_ROUTES
        .iter()
        .find(|(m, p, _)| m == method && path_matches(p, path))
        .map(|(_, _, exposure)| *exposure)
}

/// Whether a tokenless caller is answered. Pure, so no cached "onboarded" latch can make
/// `POST /onboard/reset` a one-way door (`reset_never_becomes_a_one_way_door` guards it).
fn public_without_token(exposure: Exposure, onboarded: bool, peer_is_loopback: bool) -> bool {
    match exposure {
        Exposure::Always => true,
        Exposure::UntilOnboarded => !onboarded,
        Exposure::UntilOnboardedThenHostOnly => !onboarded || peer_is_loopback,
    }
}

/// Worst case across states, for drift guards that parse `routes.rs` with no database.
/// Test-only: answering live requests from the worst case would reopen every hole.
#[cfg(test)]
fn reachable_without_token_in_some_state(method: &Method, path: &str) -> bool {
    route_exposure(method, path).is_some()
}

/// Whether the pond is set up, read live: a cache would make reset a one-way door.
/// A failed read counts as onboarded, so a transient `SQLITE_BUSY` narrows access.
async fn pond_is_onboarded(state: &crate::AppState) -> bool {
    // `--skip-onboarding` means no wizard, so it must read as onboarded, not as still running.
    if state.skip_onboarding {
        return true;
    }
    match state.onboarding_repo.is_complete().await {
        Ok(complete) => complete,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not read onboarding state; treating the pond as set up so the \
                 onboarding write holes stay shut"
            );
            true
        }
    }
}

/// Request/response logging at debug level (visible under `pond-server serve --debug`).
pub async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = std::time::Instant::now();

    tracing::debug!(
        method = %method,
        path   = %uri.path(),
        query  = %uri.query().unwrap_or(""),
        "--> incoming request"
    );

    let response = next.run(req).await;

    tracing::debug!(
        method     = %method,
        path       = %uri.path(),
        status     = response.status().as_u16(),
        latency_ms = start.elapsed().as_millis(),
        "<-- outgoing response"
    );

    response
}

/// Global bearer-token authentication; `PUBLIC_ROUTES` entries bypass it per their exposure.
pub async fn auth_middleware(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    path: axum::http::Uri,
    req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    // `ConnectInfo<SocketAddr>` comes from `main.rs`'s `into_make_service_with_connect_info`;
    // a request without it counts as remote, so failure narrows access.
    let peer_is_loopback = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().is_loopback())
        .unwrap_or(false);

    if let Some(exposure) = route_exposure(req.method(), path.path()) {
        // `Always` (every static asset, /health) must never cost a database read.
        let onboarded = exposure != Exposure::Always && pond_is_onboarded(state.as_ref()).await;
        if public_without_token(exposure, onboarded, peer_is_loopback) {
            let mut req = req;
            // `onboarded` here only for host recovery: the one privileged anonymous caller.
            // The wizard's own requests stay unattributed.
            if onboarded {
                req.extensions_mut().insert(Principal::loopback());
            }
            return Ok(next.run(req).await);
        }
    }

    // Opt-in dev bypass, else any host process reaches every protected route. It returns before
    // a token is read, so the principal carries no device.
    if dev_allow_loopback() && peer_is_loopback {
        let mut req = req;
        req.extensions_mut().insert(Principal::loopback());
        return Ok(next.run(req).await);
    }

    let token = extract_bearer_token(&headers)?;

    let valid = state
        .handshake
        .validate_token(&token)
        .await
        .map_err(|_| AuthError::InvalidToken)?;

    if !valid {
        return Err(AuthError::InvalidToken);
    }

    // The device id comes only from `caller_for_token`: a client-supplied one would outrank every
    // proof (`PairedDevice` beats face and explicit id). `device_rung_wiring.rs` guards this.
    let mut principal = match state.handshake.caller_for_token(&token).await {
        Ok(Some(caller)) => Principal::token(caller.client_id).with_device(caller.device_id),
        _ => Principal::token("unknown".to_string()),
    };
    if let Some(ci) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        principal = principal.with_remote_addr(ci.0.to_string());
    }

    let mut req = req;
    req.extensions_mut().insert(principal);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_bearer_token_success() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer my-token-123".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers).unwrap(), "my-token-123");
    }

    #[test]
    fn test_extract_bearer_token_missing() {
        assert!(matches!(
            extract_bearer_token(&HeaderMap::new()),
            Err(AuthError::MissingToken)
        ));
    }

    #[test]
    fn test_extract_bearer_token_invalid_format() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic my-token-123".parse().unwrap());
        assert!(matches!(
            extract_bearer_token(&headers),
            Err(AuthError::InvalidFormat)
        ));
    }

    #[tokio::test]
    async fn test_rate_limiter_allows_requests_within_limit() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        for _ in 0..5 {
            assert!(limiter.check_rate_limit("client-1").await);
        }
        assert!(!limiter.check_rate_limit("client-1").await);
    }

    #[tokio::test]
    async fn rejection_reports_the_remaining_window() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check_rate_limit_detailed("client-1").await.is_ok());

        let remaining = limiter
            .check_rate_limit_detailed("client-1")
            .await
            .expect_err("second request is over the limit");
        assert!(
            remaining <= Duration::from_secs(60) && remaining > Duration::from_secs(55),
            "expected roughly the full window back, got {remaining:?}"
        );
    }

    #[test]
    fn retry_after_never_rounds_down_to_zero() {
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_secs(42)), 42);
    }

    #[test]
    fn rate_limited_response_carries_retry_after() {
        let response = AuthError::RateLimitExceeded {
            retry_after_secs: 17,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "17");
    }

    #[test]
    fn auth_failures_carry_no_retry_after() {
        let response = AuthError::InvalidToken.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::RETRY_AFTER).is_none());
    }

    #[tokio::test]
    async fn test_rate_limiter_different_clients() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.check_rate_limit("client-1").await);
        assert!(limiter.check_rate_limit("client-1").await);
        assert!(limiter.check_rate_limit("client-2").await);
        assert!(limiter.check_rate_limit("client-2").await);
    }

    #[tokio::test]
    async fn test_rate_limiter_evicts_stale_entries() {
        // Window = 1s, cleanup triggers after 2× window = 2s
        let limiter = RateLimiter::new(100, Duration::from_secs(1));
        limiter.check_rate_limit("stale-client").await;

        // Wait for the entry to go stale and cleanup to trigger
        tokio::time::sleep(Duration::from_secs(3)).await;

        // This call triggers cleanup (>2× window since last cleanup)
        limiter.check_rate_limit("new-client").await;

        let clients = limiter.clients.read().await;
        assert!(
            !clients.contains_key("stale-client"),
            "stale entry should have been evicted"
        );
        assert_eq!(clients.len(), 1, "only the fresh entry should remain");
    }

    /// The middleware's decision minus the DB read, built from the same two functions it calls.
    fn answers_without_token(
        method: &Method,
        path: &str,
        onboarded: bool,
        peer_is_loopback: bool,
    ) -> bool {
        match route_exposure(method, path) {
            Some(exposure) => public_without_token(exposure, onboarded, peer_is_loopback),
            None => false,
        }
    }

    const ONBOARDED: bool = true;
    const SETTING_UP: bool = false;
    const FROM_THE_HOST: bool = true;
    const FROM_THE_LAN: bool = false;

    #[test]
    fn test_is_public_route() {
        // Asserted in the *narrowest* state, so an entry that became state-dependent fails.
        for (method, path) in [
            (Method::GET, "/api/v1/health"),
            (Method::POST, "/api/v1/handshake"),
            (Method::POST, "/api/v1/handshake/init"),
            (Method::POST, "/api/v1/handshake/verify"),
            (Method::POST, "/api/v1/handshake/refresh"),
            (Method::POST, "/api/v1/handshake/revoke"),
            (Method::GET, "/api/v1/handshake/pairing-code"),
            (Method::GET, "/api/v1/onboard/status"),
            (Method::POST, "/api/v1/transcribe"),
            // Voice callers hit /tts tokenless after setup; closing it mutes a set-up pond.
            (Method::POST, "/api/v1/tts"),
            (Method::GET, "/api/v1/oauth/callback"),
            (Method::POST, "/api/v1/oauth/refresh"),
            (Method::GET, "/"),
            (Method::GET, "/index.html"),
            (Method::GET, "/assets/main.js"),
        ] {
            assert!(
                answers_without_token(&method, path, ONBOARDED, FROM_THE_LAN),
                "{method} {path} must answer without a token in every state -- \
                 pairing and recovery cannot depend on being paired"
            );
        }

        // Refused in the WIDEST state (mid-onboarding, on the host) means refused everywhere.
        for (method, path) in [
            (Method::POST, "/api/v1/oauth/authorize"),
            (Method::POST, "/api/v1/chat"),
            (Method::GET, "/api/v1/devices"),
        ] {
            assert!(
                !answers_without_token(&method, path, SETTING_UP, FROM_THE_HOST),
                "{method} {path} must require a token even mid-onboarding, from the host"
            );
        }
    }

    /// Same path, different methods: the allowlist must be method-scoped.
    #[test]
    fn the_pai2_p0_leaks_are_closed() {
        // GET /settings serialises all of Settings (personal config): protected in every state.
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/settings",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // ...while the wizard's PUT is open while the wizard runs.
        assert!(answers_without_token(
            &Method::PUT,
            "/api/v1/settings",
            SETTING_UP,
            FROM_THE_LAN
        ));

        // GET /profiles listed the whole household.
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/profiles",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // POST /profiles creates one during onboarding.
        assert!(answers_without_token(
            &Method::POST,
            "/api/v1/profiles",
            SETTING_UP,
            FROM_THE_LAN
        ));

        // DELETE /profiles/{id} removes a household member.
        assert!(!answers_without_token(
            &Method::DELETE,
            "/api/v1/profiles/abc-123",
            SETTING_UP,
            FROM_THE_HOST
        ));
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/profiles/abc-123",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // PATCH on the same path stays open during onboarding.
        assert!(answers_without_token(
            &Method::PATCH,
            "/api/v1/profiles/abc-123",
            SETTING_UP,
            FROM_THE_LAN
        ));
    }

    /// Being on the host is no excuse; only `/onboard/reset` gets one.
    #[test]
    fn the_onboarding_write_holes_close_once_the_pond_is_set_up() {
        for (method, path) in [
            (Method::PUT, "/api/v1/settings"),
            (Method::POST, "/api/v1/profiles"),
            (Method::PATCH, "/api/v1/profiles/abc-123"),
            (Method::POST, "/api/v1/onboard"),
            (Method::POST, "/api/v1/onboard/complete"),
            (Method::POST, "/api/v1/onboard/step/Basics"),
            (Method::POST, "/api/v1/voice/calibrate"),
            (Method::DELETE, "/api/v1/voice/calibrate"),
        ] {
            assert!(
                answers_without_token(&method, path, SETTING_UP, FROM_THE_LAN),
                "{method} {path} must be open while the wizard runs, or onboarding \
                 deadlocks on a pond nobody can finish setting up"
            );
            assert!(
                !answers_without_token(&method, path, ONBOARDED, FROM_THE_LAN),
                "{method} {path} must close once the pond is set up"
            );
            assert!(
                !answers_without_token(&method, path, ONBOARDED, FROM_THE_HOST),
                "{method} {path} must close once the pond is set up -- being on the \
                 host is a recovery excuse for /onboard/reset alone"
            );
        }

        // The status probe must stay open: tokenless clients ask it whether they need the wizard.
        assert!(answers_without_token(
            &Method::GET,
            "/api/v1/onboard/status",
            ONBOARDED,
            FROM_THE_LAN
        ));
    }

    /// With a latch instead of a live read, a reset pond's wizard stays shut until reflashed.
    #[test]
    fn reset_never_becomes_a_one_way_door() {
        // A LAN phone can't reset: that would reopen every hole above.
        assert!(
            !answers_without_token(
                &Method::POST,
                "/api/v1/onboard/reset",
                ONBOARDED,
                FROM_THE_LAN
            ),
            "an unauthenticated LAN caller must not be able to reset the pond -- \
             a reset reopens every hole this phase closes"
        );

        // The operator at the pond can: the same boundary as issuing a pairing code.
        assert!(
            answers_without_token(
                &Method::POST,
                "/api/v1/onboard/reset",
                ONBOARDED,
                FROM_THE_HOST
            ),
            "reset must stay reachable from the host, or a badly misconfigured pond \
             can only be repaired by reflashing it"
        );

        // A reset pond is a fresh install: the wizard's routes reopen to anyone.
        for (method, path) in [
            (Method::PUT, "/api/v1/settings"),
            (Method::POST, "/api/v1/profiles"),
            (Method::PATCH, "/api/v1/profiles/abc-123"),
            (Method::POST, "/api/v1/onboard/step/Basics"),
            (Method::POST, "/api/v1/onboard/complete"),
        ] {
            assert!(
                answers_without_token(&method, path, SETTING_UP, FROM_THE_LAN),
                "{method} {path} must reopen after a reset, or a reset pond can only \
                 be fixed by reflashing it"
            );
        }
    }

    /// Which routes are state-dependent is a security decision: changing one must edit this test.
    #[test]
    fn the_public_route_classification_is_pinned() {
        let mut state_dependent: Vec<String> = PUBLIC_ROUTES
            .iter()
            .filter(|(_, _, e)| *e != Exposure::Always)
            .map(|(m, p, e)| format!("{m} {p} = {e:?}"))
            .collect();
        state_dependent.sort();

        let expected = vec![
            "DELETE /voice/calibrate = UntilOnboarded".to_string(),
            // Nothing secret (phase, model name, timestamps); the boot warm-up precedes any token.
            "GET /warmup = UntilOnboarded".to_string(),
            "PATCH /profiles/{id} = UntilOnboarded".to_string(),
            // One outbound geocoding call for a place NAME; no household data leaves. Wizard-only.
            "POST /location/detect = UntilOnboarded".to_string(),
            "POST /onboard = UntilOnboarded".to_string(),
            "POST /onboard/complete = UntilOnboarded".to_string(),
            "POST /onboard/reset = UntilOnboardedThenHostOnly".to_string(),
            "POST /onboard/step/{name} = UntilOnboarded".to_string(),
            "POST /profiles = UntilOnboarded".to_string(),
            "POST /voice/calibrate = UntilOnboarded".to_string(),
            // The wizard's voice step runs before any token exists; it closes after onboarding.
            "POST /voice/tts/apply = UntilOnboarded".to_string(),
            "PUT /settings = UntilOnboarded".to_string(),
        ];

        assert_eq!(
            state_dependent, expected,
            "the set of state-dependent public routes changed. That is a security \
             decision -- if it is the right one, say so here."
        );
    }

    #[test]
    fn wildcards_match_one_segment_and_never_an_empty_one() {
        assert!(path_matches("/profiles/{id}", "/profiles/abc"));
        // One segment, not a prefix.
        assert!(!path_matches("/profiles/{id}", "/profiles/abc/secrets"));
        // A trailing slash is not an id.
        assert!(!path_matches("/profiles/{id}", "/profiles/"));
        assert!(!path_matches("/profiles/{id}", "/profiles"));
        assert!(path_matches("/onboard/step/{name}", "/onboard/step/voice"));
        assert!(path_matches("/health", "/health"));
        assert!(!path_matches("/health", "/health/sub"));
    }

    // ── Drift guards ────────────────────────────────────────────────────────
    // Both tests fail when a route in `routes.rs` lacks a public/protected decision.

    const ROUTES_RS: &str = include_str!("../routes.rs");

    fn block_between(start: &str, end: &str) -> &'static str {
        let s = ROUTES_RS
            .find(start)
            .unwrap_or_else(|| panic!("routes.rs no longer contains {start:?}"));
        let e = ROUTES_RS
            .find(end)
            .unwrap_or_else(|| panic!("routes.rs no longer contains {end:?}"));
        assert!(s < e, "{start:?} must appear before {end:?} in routes.rs");
        &ROUTES_RS[s..e]
    }

    /// Whole-identifier `get(`/`post(`/…, so neither `widget(` nor `delete_profile(` counts.
    fn method_tokens(seg: &str) -> Vec<Method> {
        let mut found = Vec::new();
        for (name, method) in [
            ("get", Method::GET),
            ("post", Method::POST),
            ("put", Method::PUT),
            ("patch", Method::PATCH),
            ("delete", Method::DELETE),
        ] {
            let needle = format!("{name}(");
            let mut from = 0;
            while let Some(i) = seg[from..].find(&needle) {
                let at = from + i;
                let boundary = seg[..at]
                    .chars()
                    .next_back()
                    .map(|c| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(true);
                if boundary {
                    found.push(method.clone());
                }
                from = at + needle.len();
            }
        }
        found
    }

    /// Every `.route("path", method(handler))` in a block, as (method, path).
    fn routes_in(block: &str) -> Vec<(Method, String)> {
        let mut out = Vec::new();
        let mut rest = block;
        while let Some(i) = rest.find(".route(") {
            rest = &rest[i + ".route(".len()..];
            let Some(q1) = rest.find('"') else { break };
            let after = &rest[q1 + 1..];
            let Some(q2) = after.find('"') else { break };
            let path = after[..q2].to_string();
            let tail = &after[q2..];
            let seg_end = tail.find(".route(").unwrap_or(tail.len());
            for m in method_tokens(&tail[..seg_end]) {
                out.push((m, path.clone()));
            }
            rest = &tail[seg_end..];
        }
        out
    }

    fn protected_block() -> &'static str {
        block_between(
            "let protected_routes = Router::new()",
            "public_routes.merge(protected_routes)",
        )
    }

    /// Exceptions are listed one by one, not by prefix, so no third `/oauth/*` route slips in.
    #[test]
    fn every_protected_route_requires_a_token() {
        let allowed_without_token: &[(Method, &str)] = &[
            // Browser redirect target: tokenless, authenticated by the PKCE state nonce.
            (Method::GET, "/oauth/callback"),
            // Extension subprocesses; checked against internal_extension_token in the handler.
            (Method::POST, "/oauth/refresh"),
        ];

        let routes = routes_in(protected_block());
        assert!(
            routes.len() > 50,
            "parsed only {} protected routes -- the parser has broken, not the router",
            routes.len()
        );

        let mut leaked = Vec::new();
        for (method, path) in &routes {
            let full = format!("/api/v1{path}");
            // No pond state here, so ask the safe question: anonymous in SOME state means it leaks.
            if !reachable_without_token_in_some_state(method, &full) {
                continue;
            }
            let excused = allowed_without_token
                .iter()
                .any(|(m, p)| m == method && p == path);
            if !excused {
                leaked.push(format!("{method} {full}"));
            }
        }
        assert!(
            leaked.is_empty(),
            "these protected routes are reachable with NO TOKEN:\n  {}\n\
             Either add the route to PUBLIC_ROUTES with a reason, or fix the allowlist.",
            leaked.join("\n  ")
        );
    }

    /// A public route missing from the allowlist 401s; an extra entry is a stale exemption.
    #[test]
    fn public_router_and_allowlist_agree() {
        let router: std::collections::BTreeSet<String> = routes_in(block_between(
            "let public_routes = Router::new()",
            "let protected_routes = Router::new()",
        ))
        .iter()
        .map(|(m, p)| format!("{m} {p}"))
        .collect();

        // oauth entries are covered by `every_protected_route_requires_a_token` instead.
        let allowlist: std::collections::BTreeSet<String> = PUBLIC_ROUTES
            .iter()
            .filter(|(_, p, _)| !p.starts_with("/oauth/"))
            .map(|(m, p, _)| format!("{m} {p}"))
            .collect();

        let missing: Vec<_> = router.difference(&allowlist).collect();
        let extra: Vec<_> = allowlist.difference(&router).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "public router and PUBLIC_ROUTES have drifted.\n\
             in the router, not the allowlist (these will 401 during onboarding): {missing:?}\n\
             in the allowlist, not the router (unreachable exemptions): {extra:?}"
        );
    }

    #[test]
    fn test_auth_error_to_response() {
        let err = AuthError::MissingToken;
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_loopback_bypass_is_gated_and_off_by_default() {
        // Off unless explicitly enabled — the default (env unset) must be false.
        assert!(!loopback_flag_enabled(None));
        assert!(!loopback_flag_enabled(Some("")));
        assert!(!loopback_flag_enabled(Some("0")));
        assert!(!loopback_flag_enabled(Some("yes")));
        // Only explicit truthy values enable it.
        assert!(loopback_flag_enabled(Some("1")));
        assert!(loopback_flag_enabled(Some("true")));
        assert!(loopback_flag_enabled(Some("TRUE")));
    }

    #[test]
    fn test_protected_route_without_token_is_unauthorized() {
        // A protected route is not in the public allowlist, in any state...
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/devices",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // ...and with no Authorization header the bearer extractor rejects with a 401.
        let headers = HeaderMap::new();
        let err = extract_bearer_token(&headers).expect_err("missing token must error");
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }
}
