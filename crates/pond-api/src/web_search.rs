//! The web search settings both apps use: report the state, store a key after one Brave check,
//! remove it. The key is written once and never read back by any route. Contract: section 8.4 of
//! `docs/architecture/small-model-tools.md`.

use std::sync::{Arc, OnceLock};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use pond_core::mcp::domain::tool_group::BRAVE_SEARCH_API_KEY;
use pond_core::mcp::domain::web_search::{
    local_town, WebSearchStatus, BRAVE_API_BASE, BRAVE_TIMEOUT, BRAVE_TOKEN_HEADER,
    BRAVE_WEB_SEARCH_PATH, WEB_SEARCH_PROVIDER,
};
use pond_core::mcp::services::web_search_status::{self, WebSearchStatusBoard};
use pond_core::shared::services::egress;

use crate::AppState;

/// The egress record's name for the key check, which no chat turn makes.
const KEY_CHECK_TOOL: egress::EgressPurpose = egress::EgressPurpose::WebSearchKeyCheck;

/// What a 401 or 403 from the key check answers.
const KEY_REFUSED: &str = "Brave did not accept this key.";

/// What a key that cannot travel in a request header answers.
const NOT_A_KEY: &str = "That is not a Brave key: it has characters no key contains.";

/// Where the key check goes and which status board the routes share with the tool. A request
/// carries none in a running pond, which then uses Brave and the board `search_web` writes; tests
/// layer their own on the router.
#[derive(Clone)]
pub struct WebSearchBackend {
    pub brave_base: String,
    pub board: &'static WebSearchStatusBoard,
}

impl WebSearchBackend {
    pub fn new(brave_base: impl Into<String>, board: &'static WebSearchStatusBoard) -> Self {
        Self {
            brave_base: brave_base.into(),
            board,
        }
    }
}

impl Default for WebSearchBackend {
    fn default() -> Self {
        Self::new(BRAVE_API_BASE, web_search_status::global())
    }
}

fn backend_of(layered: Option<Extension<WebSearchBackend>>) -> WebSearchBackend {
    layered.map(|Extension(b)| b).unwrap_or_default()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn no_store() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Secret storage not available",
    )
}

/// `GET /api/v1/web-search`: `{provider, configured, status, local_results: {enabled, town}}`.
/// `town` is what a search would send with local places on, so an app can say so before the
/// switch is turned; `null` when the pond has no place name.
pub async fn status(
    State(state): State<Arc<AppState>>,
    layered: Option<Extension<WebSearchBackend>>,
) -> Response {
    let backend = backend_of(layered);
    let Some(repo) = &state.secret_repo else {
        return no_store();
    };
    let stored = match repo.get(BRAVE_SEARCH_API_KEY).await {
        Ok(value) => value.filter(|k| !k.trim().is_empty()),
        Err(e) => {
            tracing::warn!(error = %e, "web search: the secret store could not be read");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The key store could not be read.",
            );
        }
    };
    let status = backend.board.status_for(stored.as_deref());
    let settings = match state.settings_repo.get().await {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!(error = %e, "web search: settings unreadable; reporting the defaults");
            Default::default()
        }
    };
    Json(json!({
        "provider": WEB_SEARCH_PROVIDER,
        "configured": stored.is_some(),
        "status": status.as_str(),
        "local_results": {
            "enabled": settings.web_search_local_results,
            "town": local_town(&settings.weather_location_name),
        },
    }))
    .into_response()
}

/// `PUT /api/v1/web-search/key` `{"key": "..."}`: one Brave request, then store the key unless
/// Brave refused it.
pub async fn set_key(
    State(state): State<Arc<AppState>>,
    layered: Option<Extension<WebSearchBackend>>,
    Json(body): Json<Value>,
) -> Response {
    let backend = backend_of(layered);
    let Some(repo) = &state.secret_repo else {
        return no_store();
    };
    let Some(key) = body
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty())
    else {
        return error(StatusCode::BAD_REQUEST, "Missing 'key' field");
    };
    // A control character fails the check before it is sent, which reads as Brave unreachable
    // and stores the key; a key is printable ASCII, so anything else is refused before Brave.
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return error(StatusCode::BAD_REQUEST, NOT_A_KEY);
    }

    let status = check_key(&backend.brave_base, key).await;
    if status == WebSearchStatus::Rejected {
        return error(StatusCode::UNPROCESSABLE_ENTITY, KEY_REFUSED);
    }
    if let Err(e) = repo.set(BRAVE_SEARCH_API_KEY, key).await {
        tracing::warn!(error = %e, "web search: the key could not be stored");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The key could not be stored.",
        );
    }
    backend.board.record(key, status);
    tracing::info!(
        target: "giap::trace",
        kind = "web_search_key_stored",
        status = status.as_str(),
        "web search key stored"
    );
    Json(json!({ "configured": true, "status": status.as_str() })).into_response()
}

/// `DELETE /api/v1/web-search/key`: 204.
pub async fn delete_key(
    State(state): State<Arc<AppState>>,
    layered: Option<Extension<WebSearchBackend>>,
) -> Response {
    let backend = backend_of(layered);
    let Some(repo) = &state.secret_repo else {
        return no_store();
    };
    if let Err(e) = repo.delete(BRAVE_SEARCH_API_KEY).await {
        tracing::warn!(error = %e, "web search: the key could not be removed");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The key could not be removed.",
        );
    }
    backend.board.clear();
    StatusCode::NO_CONTENT.into_response()
}

/// The pond's client for the key check. It follows no redirect: reqwest strips only its standard
/// credential headers on one, so the key's header would go wherever it points.
fn brave_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            pond_net::client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| tracing::warn!(error = %e, "web search: the key check client failed to build"))
                .ok()
        })
        .as_ref()
}

/// What one Brave request (`q=test`, `count=1`) says about `key`. Brave unreachable, refused by
/// the network mode, or answering something that says nothing about the key is `Unverified`:
/// the key is stored and the first search checks it.
async fn check_key(base: &str, key: &str) -> WebSearchStatus {
    let Some(client) = brave_client() else {
        return WebSearchStatus::Unverified;
    };
    let endpoint = format!("{}{BRAVE_WEB_SEARCH_PATH}", base.trim_end_matches('/'));
    let Ok(url) = reqwest::Url::parse_with_params(&endpoint, [("q", "test"), ("count", "1")])
    else {
        return WebSearchStatus::Unverified;
    };
    let call = match egress::begin_for(url.as_str(), "GET", KEY_CHECK_TOOL, "") {
        Ok(call) => call,
        Err(denied) => {
            tracing::info!(%denied, "web search: the key check was refused by the network mode");
            return WebSearchStatus::Unverified;
        }
    };
    let response = client
        .get(url)
        .header(BRAVE_TOKEN_HEADER, key)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(BRAVE_TIMEOUT)
        .send()
        .await;
    call.finish(response.as_ref().ok().map(|r| r.status().as_u16()));
    match response {
        Ok(r) => WebSearchStatus::from_brave_http(r.status().as_u16())
            .unwrap_or(WebSearchStatus::Unverified),
        Err(e) => {
            tracing::info!(
                timeout = e.is_timeout(),
                connect = e.is_connect(),
                "web search: Brave could not be reached to check the key"
            );
            WebSearchStatus::Unverified
        }
    }
}
