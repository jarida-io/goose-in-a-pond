//! The services a Pond can connect to, and a custom webhook any service can call. The list and
//! its states come from `connector_hub` over what the Pond already keeps; connecting and
//! disconnecting go through the services behind the existing routes (context sources, settings,
//! the web search key, the marketplace, OAuth), never a second copy of them.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde_json::{json, Map, Value};

use pond_adapters_uber::accounts::UberAccounts;
use pond_core::context::ports::ContextRepository;
use pond_core::mcp::ports::extension_manager::ExtensionInfo;
use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::connector::{built_in_manifests, ConnectVia, ConnectorManifest};
use pond_core::shared::ports::hook_event_sink::{HookEventSink, NewHookEvent};
use pond_core::shared::services::connector_hub::{
    self, AccountSource, ConnectorWorld, UberState, WebhookView,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::services::oauth_providers::builtin_oauth_providers;
use pond_infra::sqlite_connector_webhooks::{hash_token, SqliteConnectorWebhooks};
use pond_infra::sqlite_hook_event_sink::SqliteHookEventSink;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::network::{CompanionTransport, EmbeddedAddress};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// The largest body a webhook call may send.
pub(crate) const HOOK_BODY_LIMIT: usize = 64 * 1024;
/// Calls one webhook may make in [`HOOK_WINDOW`]: a chatty service is slowed, not the Pond.
const HOOK_CALLS_PER_WINDOW: usize = 30;
const HOOK_WINDOW: Duration = Duration::from_secs(60);
/// The companion listener's first port, for a remote address when no listener reported its own.
const HTTPS_PORT: u16 = 4443;

/// Kept per token hash, so a flood on one webhook never slows another.
static HOOK_LIMITER: LazyLock<crate::middleware::RateLimiter> =
    LazyLock::new(|| crate::middleware::RateLimiter::new(HOOK_CALLS_PER_WINDOW, HOOK_WINDOW));

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/connectors", get(list))
        .route("/me/connectors/webhook", post(create_webhook))
        .route("/me/connectors/{id}/connect", post(connect))
        .route("/me/connectors/{id}/disconnect", post(disconnect))
}

type Caller = Option<Extension<Principal>>;

fn manifests() -> Vec<ConnectorManifest> {
    built_in_manifests()
}

fn webhooks(state: &AppState) -> SqliteConnectorWebhooks {
    SqliteConnectorWebhooks::new(state.db.system.clone())
}

/// Everything the states are read from, for `member`. A part the Pond cannot read is left
/// empty rather than failing the list.
pub(crate) async fn world(state: &Arc<AppState>, member: &str) -> Result<ConnectorWorld, Failure> {
    let settings = state.settings_repo.get().await.map_err(server_error)?;
    let accounts = crate::routes::context_repo(state)
        .list_sources(&ProfileScope::Owner(member.to_string()))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.profile_id() == member && s.kind().needs_credentials())
        .map(|s| AccountSource {
            id: s.id().to_string(),
            kind: s.kind().as_str().to_string(),
            provider: s.provider().to_string(),
            status: s.status().as_str().to_string(),
            last_sync: s.last_sync(),
        })
        .collect();
    let extensions = match &state.extension_manager {
        Some(manager) => {
            let mut live = manager.list_extensions().await.unwrap_or_default();
            if let Some(saved) = &state.mcp_server_repo {
                let names: HashSet<String> = live.iter().map(|e| e.name.clone()).collect();
                for server in saved.list().await.unwrap_or_default() {
                    if !names.contains(&server.name) {
                        live.push(ExtensionInfo {
                            name: server.name,
                            kind: server.kind,
                            description: server.description,
                            tools: Vec::new(),
                            enabled: server.enabled,
                            status: if server.enabled { "error" } else { "disabled" }.into(),
                            last_error: server.enabled.then(|| "It failed to start.".to_string()),
                            status_reason: None,
                        });
                    }
                }
            }
            Some(live)
        }
        None => None,
    };
    let marketplace = match &state.marketplace {
        Some(mp) => mp.list_available().await.unwrap_or_default(),
        None => Vec::new(),
    };
    let mut oauth_signed_in = HashSet::new();
    if let Some(secrets) = &state.secret_repo {
        for provider in builtin_oauth_providers() {
            for key in [&provider.token_key, &provider.refresh_key] {
                if secrets.has(key).await.unwrap_or(false) {
                    oauth_signed_in.insert(provider.id.clone());
                }
            }
        }
    }
    let uber = match &state.secret_repo {
        None => UberState::Unreadable,
        Some(secrets) => match UberAccounts::new(secrets.clone(), None)
            .connected_members()
            .await
        {
            Ok(members) if members.iter().any(|m| m == member) => UberState::Connected,
            Ok(_) if crate::musickit::managed_url().is_none() => UberState::SignInOff,
            Ok(_) => UberState::NotConnected,
            Err(_) => UberState::Unreadable,
        },
    };
    let matter = match &state.matter {
        Some(matter) => Some(matter.status().await),
        None => None,
    };
    let webhooks = webhooks(state)
        .list_for(member)
        .await
        .map_err(server_error)?
        .into_iter()
        .map(|w| WebhookView {
            id: w.id,
            name: w.name,
            created_at: w.created_at,
            last_event_at: w.last_event_at,
        })
        .collect();
    Ok(ConnectorWorld {
        settings,
        secret_store: state.secret_repo.is_some(),
        web_search: crate::web_search::built_in_state(state).await,
        accounts,
        extensions,
        marketplace,
        oauth_signed_in,
        uber,
        matter,
        webhooks,
        now: Utc::now(),
    })
}

/// `GET /me/connectors` → `{"connectors": [Connector]}`.
async fn list(
    State(state): State<Arc<AppState>>,
    principal: Caller,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let world = world(&state, &member).await?;
    Ok(Json(
        json!({ "connectors": connector_hub::connectors(&manifests(), &world) }),
    ))
}

/// The connector `id` as it stands now, or 404.
async fn current(state: &Arc<AppState>, member: &str, id: &str) -> Result<Value, Failure> {
    let world = world(state, member).await?;
    connector_hub::find(&manifests(), &world, id)
        .map(|c| json!(c))
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such connector"))
}

/// How a connector id is connected: its manifest's way, or the one an instance's id implies.
fn via_of(id: &str) -> Option<(ConnectVia, Option<String>)> {
    if let Some(manifest) = manifests().into_iter().find(|m| m.id == id) {
        return Some((manifest.connect, None));
    }
    let (prefix, rest) = id.split_once(':')?;
    let via = match prefix {
        "mcp" => ConnectVia::Marketplace {
            extension: rest.to_string(),
        },
        "mail" | "calendar" => ConnectVia::ContextSource {
            source_kind: prefix.to_string(),
        },
        "webhook" => ConnectVia::Webhook,
        _ => return None,
    };
    Some((via, Some(rest.to_string())))
}

/// A delegated route's refusal, passed on as it was given.
async fn passed_on(response: Response) -> Result<Value, Failure> {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .map_err(server_error)?;
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if status.is_success() {
        Ok(value)
    } else {
        Err((status, Json(value)))
    }
}

fn text_field(fields: &Map<String, Value>, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn missing(names: &[&str]) -> Failure {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"error": "some fields are missing", "missing": names})),
    )
}

/// Switches settings through the settings route, so its checks and side effects all run.
async fn switch(
    state: &Arc<AppState>,
    principal: &Caller,
    names: &[String],
    on: bool,
) -> Result<(), Failure> {
    let patch: Map<String, Value> = names.iter().map(|n| (n.clone(), json!(on))).collect();
    crate::routes::update_settings(
        State(state.clone()),
        principal.clone(),
        Ok(Json(Value::Object(patch))),
    )
    .await
    .map(|_| ())
}

/// `POST /me/connectors/{id}/connect` `{"fields": {...}}` → Connector. A sign-in that needs a
/// browser answers with its address in `setup.oauth`.
async fn connect(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let fields = body
        .and_then(|Json(b)| b.get("fields").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    let Some((via, instance)) = via_of(&id) else {
        return Err(failure(StatusCode::NOT_FOUND, "no such connector"));
    };
    let mut oauth = None;
    let shown_id = match (via, instance) {
        // An account or a webhook that already exists is connected already.
        (ConnectVia::ContextSource { .. } | ConnectVia::Webhook, Some(_)) => id.clone(),
        (
            ConnectVia::Setting {
                setting, mut also, ..
            },
            _,
        ) => {
            also.insert(0, setting);
            switch(&state, &principal, &also, true).await?;
            id.clone()
        }
        (ConnectVia::Matter { setting }, _) => {
            switch(&state, &principal, &[setting], true).await?;
            id.clone()
        }
        (ConnectVia::ContextSource { source_kind }, None) => {
            let (provider, username, password) = (
                text_field(&fields, "provider"),
                text_field(&fields, "username"),
                text_field(&fields, "password"),
            );
            let (Some(provider), Some(username), Some(password)) = (provider, username, password)
            else {
                return Err(missing(&["provider", "username", "password"]));
            };
            let request = serde_json::from_value(json!({
                "kind": source_kind,
                "provider": provider,
                "session_id": "",
                "credentials": {
                    "username": username,
                    "password": password,
                    "base_url": text_field(&fields, "base_url"),
                },
            }))
            .map_err(server_error)?;
            let Json(made) = crate::routes::connect_context_source(
                State(state.clone()),
                principal,
                Json(request),
            )
            .await?;
            if made["profile_id"] != json!(member) {
                return Err(failure(
                    StatusCode::FORBIDDEN,
                    "that account would belong to another member",
                ));
            }
            format!("{source_kind}:{}", made["id"].as_str().unwrap_or_default())
        }
        (ConnectVia::WebSearchKey, _) => {
            let Some(key) = text_field(&fields, "key") else {
                return Err(missing(&["key"]));
            };
            let response =
                crate::web_search::set_key(State(state.clone()), None, Json(json!({"key": key})))
                    .await;
            passed_on(response).await?;
            id.clone()
        }
        (ConnectVia::Uber, _) => {
            let mut shown = current(&state, &member, &id).await?;
            shown["status_detail"] =
                json!("Connect Uber on the Pond itself, in Settings, under Accounts.");
            return Err((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "Uber is connected on the Pond itself, in Settings, under Accounts.",
                    "connector": shown,
                })),
            ));
        }
        (ConnectVia::Oauth { provider }, _) => {
            let response = crate::routes::oauth_authorize_handler(
                State(state.clone()),
                headers,
                Json(json!({"provider": provider})),
            )
            .await;
            let started = passed_on(response).await?;
            oauth = started["auth_url"].as_str().map(str::to_string);
            id.clone()
        }
        (ConnectVia::Webhook, None) => {
            let Some(name) = text_field(&fields, "name") else {
                return Err(missing(&["name"]));
            };
            let (hook, _token) = webhooks(&state)
                .create(&member, &name, Utc::now())
                .await
                .map_err(server_error)?;
            format!("webhook:{}", hook.id)
        }
        (ConnectVia::Marketplace { extension }, _) => {
            let installed = match &state.extension_manager {
                Some(manager) => manager
                    .list_extensions()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|e| e.name == extension),
                None => false,
            } || match &state.mcp_server_repo {
                Some(saved) => saved
                    .list()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|s| s.name == extension),
                None => false,
            };
            let response = if installed {
                crate::routes::toggle_extension_handler(
                    State(state.clone()),
                    Path(extension),
                    Ok(Json(json!({"enabled": true}))),
                )
                .await
            } else {
                crate::routes::install_marketplace_handler(
                    State(state.clone()),
                    Path(extension),
                    Some(Json(json!({"secrets": fields}))),
                )
                .await
            };
            passed_on(response).await?;
            id.clone()
        }
    };
    let mut shown = current(&state, &member, &shown_id).await?;
    if let Some(url) = oauth {
        shown["status"] = json!("needs_setup");
        shown["setup"]["oauth"] = json!(url);
        shown["status_detail"] = json!(
            "Open the sign-in page in a browser on the Pond itself; it comes back there when you \
             finish."
        );
    }
    Ok(Json(shown))
}

/// `POST /me/connectors/{id}/disconnect` → Connector. An account or a webhook goes, and the
/// connector to add another is answered; a switch or an extension is turned off.
async fn disconnect(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Some((via, instance)) = via_of(&id) else {
        return Err(failure(StatusCode::NOT_FOUND, "no such connector"));
    };
    let shown_id = match (via, instance) {
        (ConnectVia::Setting { setting, .. }, _) | (ConnectVia::Matter { setting }, _) => {
            switch(&state, &principal, &[setting], false).await?;
            id.clone()
        }
        (ConnectVia::ContextSource { source_kind }, Some(source)) => {
            let scope = ProfileScope::Owner(member.clone());
            let repo = crate::routes::context_repo(&state);
            let mine = repo
                .get_source(&source, &scope)
                .await
                .map_err(server_error)?
                .is_some_and(|s| s.profile_id() == member);
            if !mine {
                return Err(failure(StatusCode::NOT_FOUND, "no such connector"));
            }
            repo.disconnect_source(&source, &scope)
                .await
                .map_err(server_error)?;
            source_kind
        }
        (ConnectVia::ContextSource { .. } | ConnectVia::Webhook, None) => {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "disconnect one account or webhook, by its own id",
            ))
        }
        (ConnectVia::Webhook, Some(hook)) => {
            if !webhooks(&state)
                .delete(&member, &hook)
                .await
                .map_err(server_error)?
            {
                return Err(failure(StatusCode::NOT_FOUND, "no such connector"));
            }
            "webhook".to_string()
        }
        (ConnectVia::WebSearchKey, _) => {
            passed_on(crate::web_search::delete_key(State(state.clone()), None).await).await?;
            id.clone()
        }
        (ConnectVia::Uber, _) => {
            return Err(failure(
                StatusCode::CONFLICT,
                "Uber is disconnected on the Pond itself, in Settings, under Accounts.",
            ))
        }
        (ConnectVia::Oauth { provider }, _) => {
            let secrets = state
                .secret_repo
                .as_ref()
                .ok_or_else(|| failure(StatusCode::SERVICE_UNAVAILABLE, "no secret store"))?;
            if let Some(p) = builtin_oauth_providers().iter().find(|p| p.id == provider) {
                for key in [&p.token_key, &p.refresh_key] {
                    secrets.delete(key).await.map_err(server_error)?;
                }
            }
            id.clone()
        }
        (ConnectVia::Marketplace { extension }, _) => {
            let response = crate::routes::toggle_extension_handler(
                State(state.clone()),
                Path(extension),
                Ok(Json(json!({"enabled": false}))),
            )
            .await;
            passed_on(response).await?;
            id.clone()
        }
    };
    Ok(Json(current(&state, &member, &shown_id).await?))
}

/// `POST /me/connectors/webhook` `{"name"}` → the Connector, plus `webhook_url`, shown once.
async fn create_webhook(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    headers: HeaderMap,
    uri: Uri,
    transport: Option<Extension<CompanionTransport>>,
    embedded: Option<Extension<EmbeddedAddress>>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Some(name) = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
    else {
        return Err(missing(&["name"]));
    };
    let name: String = name.chars().take(80).collect();
    let (hook, token) = webhooks(&state)
        .create(&member, &name, Utc::now())
        .await
        .map_err(server_error)?;
    let reachable = Reachable {
        host: headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_string)
            .or_else(|| uri.authority().map(|a| a.to_string())),
        https_port: transport.map(|Extension(t)| t.https_port),
        api_port: state.api_port,
        remote: embedded.and_then(|Extension(e)| e.0.read().ok().and_then(|a| a.clone())),
    };
    let mut shown = current(&state, &member, &format!("webhook:{}", hook.id)).await?;
    shown["webhook_url"] = json!(reachable.url_for(&token));
    Ok((StatusCode::CREATED, Json(shown)))
}

/// What the Pond knows of the address it is reached at.
struct Reachable {
    host: Option<String>,
    https_port: Option<u16>,
    api_port: u16,
    remote: Option<String>,
}

impl Reachable {
    /// The webhook's address: on the host the caller reached, else the remote-access address.
    fn url_for(&self, token: &str) -> String {
        let path = format!("/api/v1/hooks/in/{token}");
        if let Some(host) = self.host.as_deref().filter(|h| !h.is_empty()) {
            let port = host
                .rsplit_once(':')
                .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|(_, p)| p.parse::<u16>().ok());
            let plain = port.is_some_and(|p| p == self.api_port && Some(p) != self.https_port);
            let scheme = if plain { "http" } else { "https" };
            return format!("{scheme}://{host}{path}");
        }
        match &self.remote {
            Some(remote) => format!(
                "https://{remote}:{}{path}",
                self.https_port.unwrap_or(HTTPS_PORT)
            ),
            None => path,
        }
    }
}

/// `POST /hooks/in/{token}`: a webhook connector's call. Public, as the token is the credential;
/// any JSON body, up to [`HOOK_BODY_LIMIT`], becomes a `hook_events` row → 202.
pub(crate) async fn hook_in(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    let key = hash_token(&token);
    if let Err(wait) = HOOK_LIMITER.check_rate_limit_detailed(&key).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, wait.as_secs().max(1).to_string())],
            Json(json!({"error": "too many calls; wait a minute"})),
        )
            .into_response();
    }
    let payload: Value = if body.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => return failure(StatusCode::BAD_REQUEST, "send a JSON body").into_response(),
        }
    };
    let hooks = webhooks(&state);
    let hook = match hooks.by_token(&token).await {
        Ok(Some(hook)) => hook,
        Ok(None) => return failure(StatusCode::NOT_FOUND, "no such webhook").into_response(),
        Err(e) => return server_error(e).into_response(),
    };
    let now = Utc::now();
    let event = NewHookEvent::new(
        Some(&hook.owner),
        "connector",
        &format!("webhook:{}", hook.id),
        "call",
        payload,
        now,
    );
    let sink = SqliteHookEventSink::new(state.db.system.clone());
    if let Err(e) = sink.record(event).await {
        return server_error(e).into_response();
    }
    if let Err(e) = hooks.touch(&hook.id, now).await {
        tracing::warn!(target: "giap::trace", kind = "webhook_touch_failed", error = %e);
    }
    tracing::info!(target: "giap::trace", kind = "webhook_call", bytes = body.len());
    (StatusCode::ACCEPTED, Json(json!({"accepted": true}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_webhook_address_is_built_from_where_the_pond_is_reached() {
        let token = "t".repeat(43);
        let lan = Reachable {
            host: Some("192.168.1.20:4443".into()),
            https_port: Some(4443),
            api_port: 4000,
            remote: Some("100.101.102.103".into()),
        };
        assert_eq!(
            lan.url_for(&token),
            format!("https://192.168.1.20:4443/api/v1/hooks/in/{token}")
        );
        let loopback = Reachable {
            host: Some("127.0.0.1:4000".into()),
            ..lan
        };
        assert_eq!(
            loopback.url_for(&token),
            format!("http://127.0.0.1:4000/api/v1/hooks/in/{token}")
        );
        let hostless = Reachable {
            host: None,
            ..loopback
        };
        assert_eq!(
            hostless.url_for(&token),
            format!("https://100.101.102.103:4443/api/v1/hooks/in/{token}")
        );
        let nowhere = Reachable {
            remote: None,
            ..hostless
        };
        assert_eq!(nowhere.url_for(&token), format!("/api/v1/hooks/in/{token}"));
    }

    #[test]
    fn an_instance_id_says_how_it_is_connected() {
        assert!(matches!(
            via_of("mail:mail:gmail:jerry"),
            Some((ConnectVia::ContextSource { .. }, Some(source))) if source == "mail:gmail:jerry"
        ));
        assert!(matches!(
            via_of("webhook:w1"),
            Some((ConnectVia::Webhook, Some(id))) if id == "w1"
        ));
        assert!(matches!(
            via_of("mcp:github"),
            Some((ConnectVia::Marketplace { extension }, Some(_))) if extension == "github"
        ));
        assert!(matches!(
            via_of("weather"),
            Some((ConnectVia::Setting { .. }, None))
        ));
        assert!(via_of("nonsense").is_none());
    }
}
