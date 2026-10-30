//! Authentication, rate limiting, onboarding, and debug-logging middleware.

pub mod onboarding_guard;

use axum::{
    extract::{Request, State},
    http::{header, header::HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pond_core::security::ports::policy::Principal;
use pond_core::security::ports::remote_access::{RemoteMachines, TailnetMachine};
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
    /// A tailnet request whose token belongs to a device enrolled on another machine.
    WrongMachine,
    /// The Pond could not learn which machine the token's device enrolled; retrying may work.
    MachineUnverifiable,
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
            AuthError::WrongMachine => (
                StatusCode::FORBIDDEN,
                "This token belongs to a device enrolled on another machine",
            ),
            AuthError::MachineUnverifiable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Could not confirm which machine this device enrolled; try again",
            ),
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
pub fn dev_allow_loopback() -> bool {
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
    /// Public in every state: pairing, health, onboarding status and connection discovery.
    Always,
    /// Only local compatibility callers may omit credentials.
    HostOnly,
    /// Authentication is required even before onboarding completes.
    Authenticated,
    /// The wizard's own surface. Tokenless only from loopback while the pond is neither claimed
    /// nor onboarded (the desktop before it pairs); once claimed, the claimant runs it as itself.
    UntilOnboarded,
    /// As [`Exposure::UntilOnboarded`], but still open to loopback once set up.
    /// Only `POST /onboard/reset`, for repair; its handler then asks a caller with no token for
    /// the host credential, the same proof re-pairing needs.
    UntilOnboardedThenHostOnly,
}

/// What a state-dependent route's tokenless answer depends on, read live on each request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PondSetup {
    onboarded: bool,
    claimed: bool,
}

/// Every pre-onboarding `(method, path, exposure)`; `{brace}` is one segment. Must match the
/// public router in `routes::api_routes` (`public_router_and_allowlist_agree`).
const PUBLIC_ROUTES: &[(Method, &str, Exposure)] = &[
    (Method::GET, "/health", Exposure::Always),
    // Pairing precedes any token; the pairing-code routes are loopback-gated in their handlers.
    (Method::POST, "/handshake", Exposure::Always),
    (Method::POST, "/handshake/pake/init", Exposure::Always),
    (Method::POST, "/handshake/pake/verify", Exposure::Always),
    (Method::POST, "/handshake/init", Exposure::Always),
    (Method::POST, "/handshake/verify", Exposure::Always),
    (Method::POST, "/handshake/refresh", Exposure::Always),
    (Method::POST, "/handshake/revoke", Exposure::Authenticated),
    (Method::GET, "/handshake/pairing-code", Exposure::Always),
    (Method::POST, "/handshake/pairing-code", Exposure::Always),
    // A headless pond's way to pair a phone: LAN-gated in the handler, never returns the code.
    (Method::POST, "/handshake/spoken-code", Exposure::Always),
    // Onboarding: tokenless from an unclaimed pond's host, else the claimant's token, never after.
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
    // GET serialises all Settings, so it is never tokenless; a paired device reads it during the
    // wizard, as the PUT's own answer already lets it.
    (Method::PUT, "/settings", Exposure::UntilOnboarded),
    (Method::GET, "/settings", Exposure::Authenticated),
    // Warm-up banner during the wizard; afterwards `PondApiClient.get` sends the bearer token.
    (Method::GET, "/warmup", Exposure::UntilOnboarded),
    // Local Piper, no user data. The dashboard's voice preview and `playTtsSentence` may call it
    // without a token; the handler then asks for the host credential.
    (Method::POST, "/tts", Exposure::HostOnly),
    // Wizard voice setup; afterwards PondApiClient.ts sends the token (verified, unlike /tts).
    (Method::POST, "/voice/tts/apply", Exposure::UntilOnboarded),
    (Method::POST, "/voice/calibrate", Exposure::UntilOnboarded),
    (Method::DELETE, "/voice/calibrate", Exposure::UntilOnboarded),
    // Speaks on the pond's speaker, so never tokenless; public only to skip the onboarding guard.
    (Method::POST, "/voice/sample", Exposure::Authenticated),
    // Discovery needs system info before pairing; transcription and diagnostics stay protected.
    (Method::GET, "/system/info", Exposure::Always),
    // Names the radios and why they are off; nothing a stranger in range should read.
    (Method::GET, "/network/status", Exposure::Authenticated),
    // The connections settings and the setup card's state. Never tokenless: the writes can move
    // the pond off the network, and their handlers also ask who the caller is.
    (Method::GET, "/connections", Exposure::Authenticated),
    (
        Method::GET,
        "/connections/wifi/networks",
        Exposure::Authenticated,
    ),
    (
        Method::POST,
        "/connections/wifi/join",
        Exposure::Authenticated,
    ),
    (
        Method::DELETE,
        "/connections/wifi/networks/{ssid}",
        Exposure::Authenticated,
    ),
    (Method::GET, "/setup/status", Exposure::Authenticated),
    (Method::GET, "/test", Exposure::HostOnly),
    // Speaks on the host's speaker; the handler also asks for the host credential.
    (Method::POST, "/test/speak", Exposure::HostOnly),
    // Onboarding creates/patches; GET /profiles and DELETE /profiles/{id} are deliberately absent.
    (Method::POST, "/profiles", Exposure::UntilOnboarded),
    (Method::PATCH, "/profiles/{id}", Exposure::UntilOnboarded),
    // Completion needs a chat model, so a paired device must reach these before onboarding (#365).
    // Never tokenless: downloads spend the household's bandwidth and disk. The rest of /models
    // (search, delete, cleanup, scan, ollama pull) stays behind the onboarding guard.
    (Method::GET, "/models", Exposure::Authenticated),
    (
        Method::GET,
        "/models/download/progress",
        Exposure::Authenticated,
    ),
    (
        Method::POST,
        "/models/download/control",
        Exposure::Authenticated,
    ),
    (
        Method::POST,
        "/models/{category}/{name}/download",
        Exposure::Authenticated,
    ),
    (
        Method::POST,
        "/models/{category}/{name}/activate",
        Exposure::Authenticated,
    ),
    // A paired phone's own device during owner setup: each handler admits only the caller's own
    // device. Heartbeat is not here: it names any device id it is given.
    (Method::GET, "/devices/self", Exposure::Authenticated),
    (
        Method::POST,
        "/devices/{id}/push-token",
        Exposure::Authenticated,
    ),
    (
        Method::DELETE,
        "/devices/{id}/push-token",
        Exposure::Authenticated,
    ),
    (
        Method::GET,
        "/notifications/stream",
        Exposure::Authenticated,
    ),
    // The stream's fallback after a push wake; both answer from the caller's token alone.
    (
        Method::GET,
        "/notifications/pending",
        Exposure::Authenticated,
    ),
    (
        Method::POST,
        "/notifications/{id}/ack",
        Exposure::Authenticated,
    ),
    // The household's notice history, read and marked by the caller's own device, from its token.
    (
        Method::GET,
        "/notifications/history",
        Exposure::Authenticated,
    ),
    (
        Method::POST,
        "/notifications/history/read",
        Exposure::Authenticated,
    ),
    // Protected-router routes guarded by other means: the redirect target, by its PKCE nonce...
    (Method::GET, "/oauth/callback", Exposure::Always),
    // ...and extensions' refresh, checked against internal_extension_token in the handler.
    (Method::POST, "/oauth/refresh", Exposure::Always),
    // The music player bridge. Extension subprocesses call these with the internal token,
    // checked in the handler. HostOnly: every legitimate caller dials 127.0.0.1 by construction.
    // `POST /player/egress-policy` is not here: its only caller, the `/player.html` page, holds
    // a session like any other client.
    (Method::POST, "/player/command", Exposure::HostOnly),
    (Method::GET, "/player/status", Exposure::HostOnly),
    (Method::POST, "/extension/egress", Exposure::HostOnly),
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
    // Dashboard and dev pages: tokenless only from loopback, never via a forwarding header.
    if !path.starts_with("/api/") {
        return Some(Exposure::HostOnly);
    }

    let path = path.strip_prefix("/api/v1").unwrap_or(path);

    PUBLIC_ROUTES
        .iter()
        .find(|(m, p, _)| m == method && path_matches(p, path))
        .map(|(_, _, exposure)| *exposure)
}

/// Whether a tokenless caller is answered: the table in `pond-networking.md`, "Claim before
/// onboarding". Pure, so no cached latch can make `POST /onboard/reset` a one-way door
/// (`reset_never_becomes_a_one_way_door` guards it).
fn public_without_token(exposure: Exposure, pond: PondSetup, peer_is_loopback: bool) -> bool {
    match exposure {
        Exposure::Always => true,
        Exposure::HostOnly => peer_is_loopback,
        Exposure::Authenticated => false,
        Exposure::UntilOnboarded => peer_is_loopback && !pond.claimed && !pond.onboarded,
        Exposure::UntilOnboardedThenHostOnly => {
            peer_is_loopback && (!pond.claimed || pond.onboarded)
        }
    }
}

/// Worst case across states, for drift guards that parse `routes.rs` with no database.
/// Test-only: answering live requests from the worst case would reopen every hole.
#[cfg(test)]
fn reachable_without_token_in_some_state(method: &Method, path: &str) -> bool {
    route_exposure(method, path).is_some_and(|e| e != Exposure::Authenticated)
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

/// Whether a device has claimed the pond, read live for the reason onboarding is. A failed read
/// counts as claimed, which narrows access; a pond with no setup record reads as unclaimed.
async fn pond_is_claimed(state: &crate::AppState) -> bool {
    let Some(claims) = state.setup_claims.as_ref() else {
        return false;
    };
    match claims.claim_state().await {
        Ok(claim) => claim.is_claimed(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not read the claim state; treating the pond as claimed so its wizard \
                 needs a paired device's token"
            );
            true
        }
    }
}

/// GETs an `<img>` tag or MJPEG viewer makes, which cannot set an `Authorization` header.
const QUERY_TOKEN_ROUTES: &[&str] = &[
    "/camera/snapshot",
    "/camera/{camera_id}/snapshot",
    "/camera/stream",
    "/camera/{camera_id}/stream",
    "/camera/events/{id}/snapshot",
];

/// Whether `?token=` may stand in for the bearer header on this request.
fn accepts_query_token(method: &Method, path: &str) -> bool {
    *method == Method::GET
        && path.strip_prefix("/api/v1").is_some_and(|path| {
            QUERY_TOKEN_ROUTES
                .iter()
                .any(|pattern| path_matches(pattern, path))
        })
}

fn query_key(pair: &str) -> &str {
    pair.split_once('=').map_or(pair, |(key, _)| key)
}

/// The percent-decoded `token` query parameter, if present and non-empty.
fn query_token(query: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key != "token" {
            return None;
        }
        urlencoding::decode(value)
            .ok()
            .map(|t| t.into_owned())
            .filter(|t| !t.is_empty())
    })
}

/// Credentials in a query are not logged: a sign-in callback's one-time code and state, and any
/// `token` parameter.
fn loggable_query(uri: &axum::http::Uri) -> std::borrow::Cow<'_, str> {
    if uri.path().ends_with("/oauth/callback") {
        return "<redacted>".into();
    }
    let query = uri.query().unwrap_or("");
    if !query.split('&').any(|pair| query_key(pair) == "token") {
        return query.into();
    }
    query
        .split('&')
        .map(|pair| {
            if query_key(pair) == "token" {
                "token=<redacted>"
            } else {
                pair
            }
        })
        .collect::<Vec<_>>()
        .join("&")
        .into()
}

/// Request/response logging at debug level (visible under `pond-server serve --debug`).
pub async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = std::time::Instant::now();

    tracing::debug!(
        method = %method,
        path   = %uri.path(),
        query  = %loggable_query(&uri),
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

/// Bearer-token auth; a `PUBLIC_ROUTES` entry skips it only if its exposure admits the peer.
pub async fn auth_middleware(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    path: axum::http::Uri,
    req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    // `ConnectInfo<SocketAddr>` comes from `main.rs`'s `into_make_service_with_connect_info`;
    // a request without it counts as remote, so failure narrows access.
    // A request on the plaintext development listener is never the host's own: a browser on
    // this machine reaches that listener as a loopback peer too. So it gets neither host-only
    // routes nor the dev bypass below.
    let peer_is_loopback = req
        .extensions()
        .get::<crate::insecure_dev::InsecureDevTransport>()
        .is_none()
        && req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(false);

    if let Some(exposure) = route_exposure(req.method(), path.path()) {
        // Only the state-dependent classes read pond state; the rest ignore it.
        let state_dependent = matches!(
            exposure,
            Exposure::UntilOnboarded | Exposure::UntilOnboardedThenHostOnly
        );
        let onboarded = state_dependent && pond_is_onboarded(state.as_ref()).await;
        // The claim changes the answer only for a loopback peer before onboarding.
        let claimed = state_dependent
            && peer_is_loopback
            && !onboarded
            && pond_is_claimed(state.as_ref()).await;
        let pond = PondSetup { onboarded, claimed };
        if public_without_token(exposure, pond, peer_is_loopback) {
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

    let token = extract_bearer_token(&headers).or_else(|missing| {
        if accepts_query_token(req.method(), path.path()) {
            req.uri().query().and_then(query_token).ok_or(missing)
        } else {
            Err(missing)
        }
    })?;

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
    let (mut principal, principal_device) = match state.handshake.caller_for_token(&token).await {
        Ok(Some(caller)) => {
            // Cloned first: `device_rung_wiring` needs bare `caller.device_id` in the call below.
            let on_lan = caller.device_id.clone();
            (
                Principal::token(caller.client_id).with_device(caller.device_id),
                on_lan,
            )
        }
        _ => (Principal::token("unknown".to_string()), String::new()),
    };
    if let Some(ci) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        principal = principal.with_remote_addr(ci.0.to_string());
    }

    // Authenticating from the household LAN, not the tailnet, renews a device's remote access.
    if let Some(presence) = req
        .extensions()
        .get::<Arc<dyn pond_core::security::ports::remote_access::DevicePresence>>()
        .cloned()
    {
        let on_household_lan = crate::network::require_lan(
            req.extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .copied(),
        )
        .is_ok();
        if on_household_lan {
            presence.seen_on_lan(&principal_device).await;
        }
    }

    // Over the tailnet a token must come from the machine its device enrolled; otherwise a stolen
    // token would work from any machine on the tailnet.
    if let Some(TailnetMachine(machine)) = req.extensions().get::<TailnetMachine>().cloned() {
        let machines = req.extensions().get::<Arc<dyn RemoteMachines>>().cloned();
        enrolled_on(machines, &principal_device, &machine).await?;
    }

    let mut req = req;
    req.extensions_mut().insert(principal);
    Ok(next.run(req).await)
}

/// Whether `device` enrolled `machine`. Refuses when the lookup is missing or fails.
async fn enrolled_on(
    machines: Option<Arc<dyn RemoteMachines>>,
    device: &str,
    machine: &str,
) -> Result<(), AuthError> {
    let machines = machines.ok_or(AuthError::MachineUnverifiable)?;
    if device.is_empty() {
        return Err(AuthError::WrongMachine);
    }
    match machines.machine_for(device).await {
        Ok(Some(enrolled)) if enrolled == machine => Ok(()),
        Ok(enrolled) => {
            tracing::warn!(
                kind = "remote_machine_mismatch",
                device,
                enrolled = enrolled.is_some(),
                "refused a tailnet request: the token's device is not enrolled on this machine"
            );
            Err(AuthError::WrongMachine)
        }
        Err(error) => {
            tracing::warn!(%error, device, "could not look up the machine a device enrolled");
            Err(AuthError::MachineUnverifiable)
        }
    }
}

#[cfg(test)]
mod tests {
    mod tailnet_machine {
        use super::super::{enrolled_on, AuthError};
        use pond_core::security::ports::remote_access::RemoteMachines;
        use std::sync::Arc;

        const PHONE: &str = "mkey:aaaa";

        struct Enrolled(anyhow::Result<Option<String>>);

        #[async_trait::async_trait]
        impl RemoteMachines for Enrolled {
            async fn machine_for(&self, _: &str) -> anyhow::Result<Option<String>> {
                match &self.0 {
                    Ok(machine) => Ok(machine.clone()),
                    Err(error) => Err(anyhow::anyhow!("{error}")),
                }
            }
        }

        fn lookup(answer: anyhow::Result<Option<String>>) -> Option<Arc<dyn RemoteMachines>> {
            Some(Arc::new(Enrolled(answer)))
        }

        #[tokio::test]
        async fn only_the_enrolled_machine_passes() {
            assert!(enrolled_on(lookup(Ok(Some(PHONE.into()))), "phone", PHONE)
                .await
                .is_ok());
            assert!(matches!(
                enrolled_on(lookup(Ok(Some(PHONE.into()))), "phone", "mkey:bbbb").await,
                Err(AuthError::WrongMachine)
            ));
            assert!(matches!(
                enrolled_on(lookup(Ok(None)), "phone", PHONE).await,
                Err(AuthError::WrongMachine)
            ));
        }

        #[tokio::test]
        async fn a_missing_device_lookup_or_answer_refuses() {
            assert!(matches!(
                enrolled_on(lookup(Ok(Some(PHONE.into()))), "", PHONE).await,
                Err(AuthError::WrongMachine)
            ));
            assert!(matches!(
                enrolled_on(None, "phone", PHONE).await,
                Err(AuthError::MachineUnverifiable)
            ));
            assert!(matches!(
                enrolled_on(lookup(Err(anyhow::anyhow!("offline"))), "phone", PHONE).await,
                Err(AuthError::MachineUnverifiable)
            ));
        }
    }

    #[test]
    fn a_sign_in_callbacks_code_is_not_logged() {
        let callback: axum::http::Uri = "/api/v1/oauth/callback?code=secret-code&state=s"
            .parse()
            .unwrap();
        assert_eq!(super::loggable_query(&callback), "<redacted>");
        let other: axum::http::Uri = "/api/v1/chat?limit=5".parse().unwrap();
        assert_eq!(super::loggable_query(&other), "limit=5");
    }

    #[test]
    fn a_query_token_is_never_logged() {
        let snapshot: axum::http::Uri = "/api/v1/camera/porch/snapshot?ts=1&token=s3cr%2Bt&w=2"
            .parse()
            .unwrap();
        assert_eq!(
            super::loggable_query(&snapshot),
            "ts=1&token=<redacted>&w=2"
        );
        // Redacted on every route, not only the ones that accept it.
        let settings: axum::http::Uri = "/api/v1/settings?token=s3cret".parse().unwrap();
        assert_eq!(super::loggable_query(&settings), "token=<redacted>");
        let tokenish: axum::http::Uri = "/api/v1/x?tokens=1&a=token".parse().unwrap();
        assert_eq!(super::loggable_query(&tokenish), "tokens=1&a=token");
    }

    #[test]
    fn only_camera_image_and_stream_gets_take_a_query_token() {
        use axum::http::Method;
        for path in [
            "/api/v1/camera/snapshot",
            "/api/v1/camera/porch/snapshot",
            "/api/v1/camera/stream",
            "/api/v1/camera/porch/stream",
            "/api/v1/camera/events/42/snapshot",
        ] {
            assert!(super::accepts_query_token(&Method::GET, path), "{path}");
            assert!(
                !super::accepts_query_token(&Method::POST, path),
                "POST {path}"
            );
        }
        for path in [
            "/api/v1/settings",
            "/api/v1/camera/events",
            "/api/v1/vision/events/stream",
            "/api/v1/faces",
            "/api/v1/camera/porch/snapshot/extra",
            "/camera/porch/snapshot",
        ] {
            assert!(!super::accepts_query_token(&Method::GET, path), "{path}");
        }
    }

    #[test]
    fn a_query_token_is_percent_decoded() {
        assert_eq!(
            super::query_token("ts=1&token=ab%2Bc%2F%3D").as_deref(),
            Some("ab+c/=")
        );
        assert_eq!(super::query_token("token=plain").as_deref(), Some("plain"));
        assert_eq!(super::query_token("token="), None);
        assert_eq!(super::query_token("tokens=x"), None);
    }

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

    /// The middleware's decision minus the state reads, built from the same two functions it calls.
    fn answers_without_token(
        method: &Method,
        path: &str,
        pond: PondSetup,
        peer_is_loopback: bool,
    ) -> bool {
        match route_exposure(method, path) {
            Some(exposure) => public_without_token(exposure, pond, peer_is_loopback),
            None => false,
        }
    }

    /// A fresh pond nobody has paired with: the widest state.
    const SETTING_UP: PondSetup = PondSetup {
        onboarded: false,
        claimed: false,
    };
    /// A phone (or the desktop) has paired, and the wizard is not finished.
    const CLAIMED: PondSetup = PondSetup {
        onboarded: false,
        claimed: true,
    };
    const ONBOARDED: PondSetup = PondSetup {
        onboarded: true,
        claimed: true,
    };
    /// Set up before setup records existed, or re-armed afterwards.
    const ONBOARDED_UNCLAIMED: PondSetup = PondSetup {
        onboarded: true,
        claimed: false,
    };
    const EVERY_STATE: [PondSetup; 4] = [SETTING_UP, CLAIMED, ONBOARDED, ONBOARDED_UNCLAIMED];
    const FROM_THE_HOST: bool = true;
    const FROM_THE_LAN: bool = false;

    /// Every cell of the spec's table, for both classes, from the host and from the LAN.
    #[test]
    fn the_claim_before_onboarding_table_holds_cell_by_cell() {
        // (pond, peer, wizard writes, reset)
        let cells = [
            (SETTING_UP, FROM_THE_HOST, true, true),
            (SETTING_UP, FROM_THE_LAN, false, false),
            (CLAIMED, FROM_THE_HOST, false, false),
            (CLAIMED, FROM_THE_LAN, false, false),
            (ONBOARDED, FROM_THE_HOST, false, true),
            (ONBOARDED, FROM_THE_LAN, false, false),
            (ONBOARDED_UNCLAIMED, FROM_THE_HOST, false, true),
            (ONBOARDED_UNCLAIMED, FROM_THE_LAN, false, false),
        ];
        for (pond, from_host, wizard, reset) in cells {
            assert_eq!(
                public_without_token(Exposure::UntilOnboarded, pond, from_host),
                wizard,
                "wizard writes, {pond:?}, loopback={from_host}"
            );
            assert_eq!(
                public_without_token(Exposure::UntilOnboardedThenHostOnly, pond, from_host),
                reset,
                "POST /onboard/reset, {pond:?}, loopback={from_host}"
            );
        }
        // The other classes ignore pond state entirely.
        for pond in EVERY_STATE {
            for from_host in [FROM_THE_HOST, FROM_THE_LAN] {
                assert!(public_without_token(Exposure::Always, pond, from_host));
                assert!(!public_without_token(
                    Exposure::Authenticated,
                    pond,
                    from_host
                ));
                assert_eq!(
                    public_without_token(Exposure::HostOnly, pond, from_host),
                    from_host
                );
            }
        }
    }

    #[test]
    fn test_is_public_route() {
        // Asserted in the *narrowest* state, so an entry that became state-dependent fails.
        for (method, path) in [
            (Method::GET, "/api/v1/health"),
            (Method::POST, "/api/v1/handshake"),
            (Method::POST, "/api/v1/handshake/pake/init"),
            (Method::POST, "/api/v1/handshake/pake/verify"),
            (Method::POST, "/api/v1/handshake/init"),
            (Method::POST, "/api/v1/handshake/verify"),
            (Method::POST, "/api/v1/handshake/refresh"),
            (Method::GET, "/api/v1/handshake/pairing-code"),
            (Method::POST, "/api/v1/handshake/spoken-code"),
            (Method::GET, "/api/v1/onboard/status"),
            (Method::GET, "/api/v1/oauth/callback"),
            (Method::POST, "/api/v1/oauth/refresh"),
        ] {
            assert!(
                answers_without_token(&method, path, ONBOARDED, FROM_THE_LAN),
                "{method} {path} must answer without a token in every state -- \
                 pairing and recovery cannot depend on being paired"
            );
        }

        for (method, path) in [
            (Method::POST, "/api/v1/player/command"),
            (Method::GET, "/api/v1/player/status"),
            (Method::POST, "/api/v1/extension/egress"),
        ] {
            assert!(
                answers_without_token(&method, path, ONBOARDED, FROM_THE_HOST),
                "{method} {path} must reach its handler on the host"
            );
            assert!(
                !answers_without_token(&method, path, SETTING_UP, FROM_THE_LAN),
                "{method} {path} has no caller off the host"
            );
        }

        // Refused in the WIDEST state (mid-onboarding, on the host) means refused everywhere.
        for (method, path) in [
            (Method::POST, "/api/v1/handshake/revoke"),
            (Method::POST, "/api/v1/voice/sample"),
            (Method::GET, "/api/v1/settings"),
            (Method::GET, "/api/v1/models"),
            (Method::GET, "/api/v1/models/download/progress"),
            (Method::POST, "/api/v1/models/download/control"),
            (Method::POST, "/api/v1/models/gguf/gemma/download"),
            (Method::POST, "/api/v1/models/gguf/gemma/activate"),
            (Method::POST, "/api/v1/transcribe"),
            (Method::GET, "/api/v1/dev/goose"),
            (Method::POST, "/api/v1/oauth/authorize"),
            (Method::POST, "/api/v1/chat"),
            (Method::GET, "/api/v1/devices"),
            // Neighbours of the player exemptions: method- and segment-exact, no prefix. The page's
            // own routes are not exempt at all: it holds a session like any other client.
            (Method::GET, "/api/v1/player/command"),
            (Method::POST, "/api/v1/player/status"),
            (Method::POST, "/api/v1/player/command/x"),
            // The player page asks with its own session; nothing on the host needs it tokenless.
            (Method::POST, "/api/v1/player/egress-policy"),
            (Method::GET, "/api/v1/player/egress-policy"),
            (Method::POST, "/api/v1/extension/egress/x"),
            (Method::GET, "/api/v1/player/events"),
            (Method::POST, "/api/v1/player/reply"),
            (Method::GET, "/api/v1/musickit/developer-token"),
            (Method::GET, "/api/v1/player/user-token"),
            (Method::GET, "/api/v1/secrets"),
            // Web search settings: the same door as `/secrets`, whose key they store.
            (Method::GET, "/api/v1/web-search"),
            (Method::PUT, "/api/v1/web-search/key"),
            (Method::DELETE, "/api/v1/web-search/key"),
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
        // ...while the wizard's PUT is open to the host while the wizard runs.
        assert!(answers_without_token(
            &Method::PUT,
            "/api/v1/settings",
            SETTING_UP,
            FROM_THE_HOST
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
            FROM_THE_HOST
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
            FROM_THE_HOST
        ));
    }

    /// The wizard every client runs, desktop and phone alike.
    const WIZARD_WRITES: [(Method, &str); 11] = [
        (Method::PUT, "/api/v1/settings"),
        (Method::POST, "/api/v1/profiles"),
        (Method::PATCH, "/api/v1/profiles/abc-123"),
        (Method::POST, "/api/v1/onboard"),
        (Method::POST, "/api/v1/onboard/complete"),
        (Method::POST, "/api/v1/onboard/step/Basics"),
        (Method::POST, "/api/v1/location/detect"),
        (Method::GET, "/api/v1/warmup"),
        (Method::POST, "/api/v1/voice/tts/apply"),
        (Method::POST, "/api/v1/voice/calibrate"),
        (Method::DELETE, "/api/v1/voice/calibrate"),
    ];

    /// Being on the host is no excuse; only `/onboard/reset` gets one.
    #[test]
    fn the_onboarding_write_holes_close_once_the_pond_is_set_up() {
        for (method, path) in WIZARD_WRITES {
            assert!(
                answers_without_token(&method, path, SETTING_UP, FROM_THE_HOST),
                "{method} {path} must be open to the host while an unclaimed pond's wizard \
                 runs, or the desktop cannot set up a pond it has not paired with"
            );
            for pond in [ONBOARDED, ONBOARDED_UNCLAIMED] {
                assert!(
                    !answers_without_token(&method, path, pond, FROM_THE_LAN),
                    "{method} {path} must close once the pond is set up"
                );
                assert!(
                    !answers_without_token(&method, path, pond, FROM_THE_HOST),
                    "{method} {path} must close once the pond is set up -- being on the \
                     host is a recovery excuse for /onboard/reset alone"
                );
            }
        }

        // The status probe must stay open: tokenless clients ask it whether they need the wizard.
        for pond in EVERY_STATE {
            assert!(answers_without_token(
                &Method::GET,
                "/api/v1/onboard/status",
                pond,
                FROM_THE_LAN
            ));
        }
    }

    /// Until P4 any LAN peer could drive a fresh pond's wizard; a phone now claims it first.
    #[test]
    fn no_tokenless_lan_peer_drives_the_wizard_in_any_state() {
        for (method, path) in WIZARD_WRITES {
            for pond in EVERY_STATE {
                assert!(
                    !answers_without_token(&method, path, pond, FROM_THE_LAN),
                    "{method} {path} answered a tokenless LAN peer in {pond:?}"
                );
            }
        }
        for pond in EVERY_STATE {
            assert!(!answers_without_token(
                &Method::POST,
                "/api/v1/onboard/reset",
                pond,
                FROM_THE_LAN
            ));
        }
    }

    /// Once a device has claimed the pond, the wizard is that device's: being on the host is no
    /// longer enough, and the claimant's token is what admits it.
    #[test]
    fn a_claimed_pond_refuses_the_tokenless_host_until_it_is_set_up() {
        for (method, path) in WIZARD_WRITES {
            assert!(
                !answers_without_token(&method, path, CLAIMED, FROM_THE_HOST),
                "{method} {path} answered a tokenless host caller on a claimed pond"
            );
        }
        assert!(!answers_without_token(
            &Method::POST,
            "/api/v1/onboard/reset",
            CLAIMED,
            FROM_THE_HOST
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

        // An unclaimed reset pond is a fresh install: the wizard reopens to the host.
        for (method, path) in WIZARD_WRITES {
            assert!(
                answers_without_token(&method, path, SETTING_UP, FROM_THE_HOST),
                "{method} {path} must reopen after a reset, or a reset pond can only \
                 be fixed by reflashing it"
            );
        }

        // A claimed reset pond reopens to a paired device's token. One that has lost every token
        // is still not stuck: pairing answers in every state, so the host pairs again (as it
        // already must to re-pair), and `setup-code --rearm` returns it to the column above.
        for (method, path) in [
            (Method::GET, "/api/v1/handshake/pairing-code"),
            (Method::POST, "/api/v1/handshake/pairing-code"),
            (Method::POST, "/api/v1/handshake/init"),
            (Method::POST, "/api/v1/handshake/verify"),
            (Method::POST, "/api/v1/handshake/pake/init"),
            (Method::POST, "/api/v1/handshake/pake/verify"),
        ] {
            assert!(
                answers_without_token(&method, path, CLAIMED, FROM_THE_HOST),
                "{method} {path} must answer a claimed, reset pond's host, or a pond that lost \
                 every token can only be repaired by reflashing it"
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
            // The connections settings: a token in every state; the writes' handlers also ask
            // whether the caller is the host, the claimant or a member's device.
            "DELETE /connections/wifi/networks/{ssid} = Authenticated".to_string(),
            // A phone's own device during owner setup; each handler admits only that device.
            "DELETE /devices/{id}/push-token = Authenticated".to_string(),
            "DELETE /voice/calibrate = UntilOnboarded".to_string(),
            "GET /connections = Authenticated".to_string(),
            "GET /connections/wifi/networks = Authenticated".to_string(),
            "GET /devices/self = Authenticated".to_string(),
            // Choosing the chat model completion requires, before onboarding (#365); token only.
            "GET /models = Authenticated".to_string(),
            "GET /models/download/progress = Authenticated".to_string(),
            // The setup radios and why each is off; a token in every state, never tokenless.
            "GET /network/status = Authenticated".to_string(),
            // The household notice history: the caller's own device, from its token.
            "GET /notifications/history = Authenticated".to_string(),
            "GET /notifications/pending = Authenticated".to_string(),
            "GET /notifications/stream = Authenticated".to_string(),
            "GET /player/status = HostOnly".to_string(),
            // The wizard resumes from what it saved; the PUT's answer already shows its caller this.
            "GET /settings = Authenticated".to_string(),
            // The setup card's state, never its code.
            "GET /setup/status = Authenticated".to_string(),
            // Nothing secret (phase, model name, timestamps); the boot warm-up precedes any token.
            "GET /test = HostOnly".to_string(),
            "GET /warmup = UntilOnboarded".to_string(),
            "PATCH /profiles/{id} = UntilOnboarded".to_string(),
            "POST /connections/wifi/join = Authenticated".to_string(),
            "POST /devices/{id}/push-token = Authenticated".to_string(),
            // The music player bridge and extension egress: the handlers check the internal
            // token; HostOnly because no legitimate caller is off the host.
            "POST /extension/egress = HostOnly".to_string(),
            // One outbound geocoding call for a place NAME; no household data leaves. Wizard-only.
            "POST /handshake/revoke = Authenticated".to_string(),
            "POST /location/detect = UntilOnboarded".to_string(),
            "POST /models/download/control = Authenticated".to_string(),
            "POST /models/{category}/{name}/activate = Authenticated".to_string(),
            "POST /models/{category}/{name}/download = Authenticated".to_string(),
            "POST /notifications/history/read = Authenticated".to_string(),
            // A woken phone fetches and acknowledges its own queued notices, as the stream would.
            "POST /notifications/{id}/ack = Authenticated".to_string(),
            "POST /onboard = UntilOnboarded".to_string(),
            "POST /onboard/complete = UntilOnboarded".to_string(),
            // Once set up, the handler asks a caller with no token for the host credential.
            "POST /onboard/reset = UntilOnboardedThenHostOnly".to_string(),
            "POST /onboard/step/{name} = UntilOnboarded".to_string(),
            "POST /player/command = HostOnly".to_string(),
            "POST /profiles = UntilOnboarded".to_string(),
            // Both handlers ask for the host credential: always for speak, without a token for tts.
            "POST /test/speak = HostOnly".to_string(),
            "POST /tts = HostOnly".to_string(),
            "POST /voice/calibrate = UntilOnboarded".to_string(),
            // A paired device hears a voice before choosing it, during onboarding too.
            "POST /voice/sample = Authenticated".to_string(),
            // The wizard's voice step; tokenless only on an unclaimed pond's host, until onboarded.
            "POST /voice/tts/apply = UntilOnboarded".to_string(),
            "PUT /settings = UntilOnboarded".to_string(),
        ];

        assert_eq!(
            state_dependent, expected,
            "the set of state-dependent public routes changed. That is a security \
             decision -- if it is the right one, say so here."
        );
    }

    /// Exactly the model routes onboarding needs skip the guard (#365), and only with a token;
    /// their neighbours are not on the allowlist at all.
    #[test]
    fn only_the_model_routes_onboarding_needs_skip_the_guard() {
        for (method, path) in [
            (Method::GET, "/api/v1/models"),
            (Method::GET, "/api/v1/models/download/progress"),
            (Method::POST, "/api/v1/models/download/control"),
            (Method::POST, "/api/v1/models/gguf/gemma/download"),
            (Method::POST, "/api/v1/models/gguf/gemma/activate"),
        ] {
            assert_eq!(
                route_exposure(&method, path),
                Some(Exposure::Authenticated),
                "{method} {path}"
            );
            for pond in EVERY_STATE {
                for from_host in [FROM_THE_HOST, FROM_THE_LAN] {
                    assert!(
                        !answers_without_token(&method, path, pond, from_host),
                        "{method} {path} answered a tokenless caller in {pond:?}"
                    );
                }
            }
        }
        for (method, path) in [
            (Method::DELETE, "/api/v1/models/gguf/gemma"),
            (
                Method::POST,
                "/api/v1/models/gguf/gemma/companions/pictures",
            ),
            (Method::POST, "/api/v1/models/scan"),
            (Method::POST, "/api/v1/models/cleanup"),
            (Method::GET, "/api/v1/models/disk-usage"),
            (Method::GET, "/api/v1/models/search/gguf"),
            (Method::POST, "/api/v1/models/ollama/pull"),
            (Method::POST, "/api/v1/models/download/url"),
            (Method::POST, "/api/v1/models/registry/refresh"),
            (Method::GET, "/api/v1/models/capabilities"),
            (Method::POST, "/api/v1/models"),
            (Method::GET, "/api/v1/models/gguf/gemma/activate"),
            (Method::POST, "/api/v1/models/gguf/gemma/activate/x"),
        ] {
            assert_eq!(route_exposure(&method, path), None, "{method} {path}");
        }
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

    /// Protected-router routes whose handler authenticates the caller itself. Listed one by one,
    /// not by prefix, so no neighbouring route slips in.
    const HANDLER_AUTHENTICATED: &[(Method, &str)] = &[
        // Browser redirect target: tokenless, authenticated by the PKCE state nonce.
        (Method::GET, "/oauth/callback"),
        // Extension subprocesses; checked against internal_extension_token in the handler.
        (Method::POST, "/oauth/refresh"),
        // The music player bridge: extension subprocesses again, the internal token.
        (Method::POST, "/player/command"),
        (Method::GET, "/player/status"),
        (Method::POST, "/extension/egress"),
    ];

    #[test]
    fn every_protected_route_requires_a_token() {
        let allowed_without_token = HANDLER_AUTHENTICATED;

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

        // Protected-router entries are covered by `every_protected_route_requires_a_token`.
        let allowlist: std::collections::BTreeSet<String> = PUBLIC_ROUTES
            .iter()
            .filter(|(m, p, _)| !HANDLER_AUTHENTICATED.contains(&(m.clone(), p)))
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
