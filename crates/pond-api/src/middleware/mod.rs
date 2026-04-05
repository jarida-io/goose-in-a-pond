//! Authentication, rate limiting, and onboarding middleware.

pub mod onboarding_guard;

use axum::{
    extract::Request,
    http::{header::HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

/// Error type for authentication failures
#[derive(Debug)]
pub enum AuthError {
    MissingToken,
    InvalidFormat,
    InvalidToken,
    RateLimitExceeded,
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
            AuthError::RateLimitExceeded => (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded"),
        };
        (
            status,
            serde_json::json!({ "error": error_message, "status": status.as_u16() }).to_string(),
        )
            .into_response()
    }
}

/// Extracts Bearer token from Authorization header
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

/// Rate limiter using token bucket algorithm
pub struct RateLimiter {
    clients: Arc<RwLock<HashMap<String, ClientRateLimit>>>,
    max_requests: usize,
    window_duration: Duration,
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
        }
    }

    pub async fn check_rate_limit(&self, client_id: &str) -> bool {
        let mut clients = self.clients.write().await;
        let now = Instant::now();
        let client = clients.entry(client_id.to_string()).or_insert(ClientRateLimit {
            window_start: now,
            request_count: 0,
        });
        if now.duration_since(client.window_start) > self.window_duration {
            client.window_start = now;
            client.request_count = 0;
        }
        if client.request_count < self.max_requests {
            client.request_count += 1;
            true
        } else {
            false
        }
    }
}

/// Routes that don't require authentication
fn is_public_route(path: &str) -> bool {
    // Non-API paths are static web assets — always public
    if !path.starts_with("/api/") {
        return true;
    }

    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    matches!(
        path,
        "/health" | "/handshake" | "/onboard" | "/onboard/status" | "/transcribe"
    )
}

/// Global token-based authentication middleware.
///
/// This middleware protects all API routes by enforcing token-based
/// authentication. For every incoming request it:
///
/// 1. **Checks if the route is public** — routes like `/api/v1/health` and
///    `/api/v1/handshake` are exempt from authentication (see [`is_public_route`]).
///    This allows unauthenticated clients to perform the initial handshake and
///    health checks.
///
/// 2. **Extracts the Bearer token** from the `Authorization` header. If the
///    header is missing or malformed, the request is rejected with `401 Unauthorized`.
///
/// 3. **Validates the token** by calling `Handshake::validate_token()` on the
///    shared application state. This delegates to the active handshake
///    implementation (e.g. `MockHandshake` in dev, `GotgHandshakeAdapter` in
///    production) which checks the token against its store.  If the token is
///    invalid or expired, the request is rejected with `401 Unauthorized`.
///
/// 4. If all checks pass, the request proceeds to the next handler in the
///    middleware chain.
pub async fn auth_middleware(
    state: axum::extract::State<std::sync::Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    path: axum::http::Uri,
    req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    // 1. Public routes bypass authentication entirely.
    if is_public_route(path.path()) {
        return Ok(next.run(req).await);
    }

    // 2. Extract the bearer token from the Authorization header.
    let token = extract_bearer_token(&headers)?;

    // 3. Validate the token via the Handshake port.
    let is_valid = state
        .handshake
        .validate_token(&token)
        .await
        .unwrap_or(false);

    if !is_valid {
        return Err(AuthError::InvalidToken);
    }

    // 4. Token is valid — proceed to the next handler.
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
        assert!(matches!(extract_bearer_token(&HeaderMap::new()), Err(AuthError::MissingToken)));
    }

    #[test]
    fn test_extract_bearer_token_invalid_format() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic my-token-123".parse().unwrap());
        assert!(matches!(extract_bearer_token(&headers), Err(AuthError::InvalidFormat)));
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
    async fn test_rate_limiter_different_clients() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.check_rate_limit("client-1").await);
        assert!(limiter.check_rate_limit("client-1").await);
        assert!(limiter.check_rate_limit("client-2").await);
        assert!(limiter.check_rate_limit("client-2").await);
    }

    #[test]
    fn test_is_public_route() {
        assert!(is_public_route("/api/v1/health"));
        assert!(is_public_route("/api/v1/handshake"));
        assert!(is_public_route("/api/v1/onboard"));
        assert!(is_public_route("/api/v1/onboard/status"));
        assert!(is_public_route("/api/v1/transcribe"));
        assert!(!is_public_route("/api/v1/chat"));
        assert!(!is_public_route("/api/v1/devices"));
        assert!(!is_public_route("/api/v1/settings"));
        // Web dashboard static assets are always public
        assert!(is_public_route("/"));
        assert!(is_public_route("/index.html"));
        assert!(is_public_route("/assets/main.js"));
    }

    #[test]
    fn test_auth_error_to_response() {
        let err = AuthError::MissingToken;
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }
}
