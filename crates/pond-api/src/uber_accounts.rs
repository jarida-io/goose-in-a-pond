//! Connecting a household member's own Uber account, from the pond's desktop. Sign-in goes
//! through Jarida's credentials service, which holds the Uber app's client secret; the member's
//! tokens are kept in this pond's secret store (`pond_adapters_uber::accounts`).

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::{Extension, Json};
use pond_adapters_uber::accounts::{
    authorize_url, is_member_token_key, SignInRelay, UberAccounts, SIGN_IN_OFF,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::host_guard::HostCredential;
use crate::oauth_callback::{self, FlowOutcome, PkceSession};
use crate::AppState;

/// `PkceSession::provider_id` for an Uber sign-in, so the shared callback hands it here.
pub const PROVIDER_ID: &str = "uber";

/// The generic secrets API's `code` for a key it will not touch.
pub const SECRET_RESERVED: &str = "secret_reserved";

type Refusal = (StatusCode, Json<Value>);

/// `code` says which refusal it is, so the desktop can say what to do about it.
fn refuse(status: StatusCode, code: &str, message: &str) -> Refusal {
    (status, Json(json!({ "error": message, "code": code })))
}

/// Held while a member's Uber tokens are kept and while a member is removed, so a sign-in that
/// finishes as its member is deleted cannot leave tokens behind for nobody.
static MEMBERS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Take before removing a household member, and hold until the member is gone.
pub(crate) async fn members_gate() -> tokio::sync::MutexGuard<'static, ()> {
    MEMBERS.lock().await
}

/// Forget a member's Uber sign-in; 1 when they had one, 0 when not or with no secret store.
pub(crate) async fn forget_member(state: &AppState, profile_id: &str) -> anyhow::Result<u64> {
    let Some(secrets) = state.secret_repo.clone() else {
        return Ok(0);
    };
    let had = UberAccounts::new(secrets, None)
        .disconnect(profile_id)
        .await?;
    Ok(u64::from(had))
}

/// The generic secrets API's answer for a key holding a member's Uber tokens. A bearer alone, which
/// a paired phone has, must never read, replace or forget another member's sign-in.
pub(crate) fn refuse_reserved_secret(key: &str) -> Option<Response> {
    if key.eq_ignore_ascii_case(crate::network::NGROK_AUTHTOKEN) {
        return Some(
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "The tunnel token is managed only on the pond itself, under Remote access",
                    "code": SECRET_RESERVED,
                })),
            )
                .into_response(),
        );
    }
    is_member_token_key(key).then(|| {
        (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "Uber sign-ins belong to each member and are managed only on the pond \
                          itself, under Settings, Accounts",
                "code": SECRET_RESERVED,
            })),
        )
            .into_response()
    })
}

/// A listing of the secret store without the keys members' Uber tokens are kept under.
pub(crate) fn without_reserved_secrets(keys: Vec<String>) -> Vec<String> {
    keys.into_iter()
        .filter(|key| {
            !is_member_token_key(key) && !key.eq_ignore_ascii_case(crate::network::NGROK_AUTHTOKEN)
        })
        .collect()
}

/// Connecting someone's Uber account is for the pond's own desktop, never a paired phone.
fn host_only(
    peer: std::net::SocketAddr,
    credential: Option<&HostCredential>,
    headers: &HeaderMap,
    operation: &'static str,
) -> Result<(), Refusal> {
    if !peer.ip().is_loopback() {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "host_only",
            "Uber accounts are managed on the pond itself",
        ));
    }
    crate::host_guard::require_credential(credential, headers, operation)
}

/// The members' sign-ins in the secret store. The relay is only needed to start or renew one, so
/// listing and forgetting work with the credentials service turned off.
fn accounts(state: &AppState) -> Result<UberAccounts, Refusal> {
    let secrets = state.secret_repo.clone().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_secret_store",
            "this pond has no secret store to keep Uber sign-ins in",
        )
    })?;
    let relay =
        crate::musickit::managed_url().map(|url| SignInRelay::new(state.http_client.clone(), &url));
    Ok(UberAccounts::new(secrets, relay))
}

/// `Ok` while `profile_id` is still a household member.
async fn still_a_member(state: &AppState, profile_id: &str) -> Result<(), Refusal> {
    match state.profile_repo.get(profile_id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(refuse(
            StatusCode::NOT_FOUND,
            "not_a_member",
            "no such household member",
        )),
        Err(e) => {
            tracing::warn!(error = %e, "uber accounts: could not read the member");
            Err(refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "member_unreadable",
                "could not read the household member",
            ))
        }
    }
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub profile_id: String,
    /// The origin the desktop reaches this server at; sets where Uber sends the browser back
    /// (see [`oauth_callback::redirect_uri_for`]). Falls back to the `Origin` header.
    #[serde(default)]
    pub origin: Option<String>,
}

/// `POST /api/v1/uber/accounts/connect` — start a member's sign-in; the desktop opens `auth_url`
/// and follows the outcome at `/oauth/status/{state}`.
pub async fn connect(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Json(request): Json<ConnectRequest>,
) -> Result<Json<Value>, Refusal> {
    host_only(
        peer,
        credential.as_ref().map(|Extension(c)| c),
        &headers,
        "uber_connect",
    )?;
    let profile_id = request.profile_id.trim().to_string();
    still_a_member(&state, &profile_id).await?;

    let accounts = accounts(&state)?;
    let Some(relay) = accounts.relay() else {
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "uber_sign_in_off",
            SIGN_IN_OFF,
        ));
    };
    let client_id = relay.client_id().await.map_err(|e| {
        tracing::warn!(error = %e, "uber connect: no client id from the credentials service");
        refuse(
            StatusCode::BAD_GATEWAY,
            "uber_relay_failed",
            "Jarida's credentials service could not start an Uber sign-in",
        )
    })?;

    let nonce = oauth_callback::generate_state();
    let redirect_uri = oauth_callback::redirect_uri_for(
        crate::routes::sign_in_origin(request.origin.as_deref(), &headers),
        state.api_port,
    );
    let auth_url = authorize_url(&client_id, &redirect_uri, &nonce);
    tracing::info!(
        kind = "oauth_start",
        provider = PROVIDER_ID,
        redirect_uri = %redirect_uri,
        "OAuth sign-in started"
    );
    oauth_callback::begin_session(
        &state.oauth_state,
        nonce.clone(),
        PkceSession {
            provider_id: PROVIDER_ID.to_string(),
            // Uber documents no PKCE; the code is bound to this attempt by `state` alone.
            code_verifier: String::new(),
            redirect_uri,
            extension_id: None,
            profile_id: Some(profile_id),
            created_at: std::time::Instant::now(),
        },
    )
    .await;
    Ok(Json(json!({ "auth_url": auth_url, "state": nonce })))
}

/// `GET /api/v1/uber/accounts` — which current members have connected Uber.
pub async fn list(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Refusal> {
    host_only(
        peer,
        credential.as_ref().map(|Extension(c)| c),
        &headers,
        "uber_accounts",
    )?;
    let unreadable = |what: &str, e: anyhow::Error| {
        tracing::warn!(error = %e, "uber accounts: could not read {what}");
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "uber_accounts_unreadable",
            "could not read Uber connections",
        )
    };
    let connected = accounts(&state)?
        .connected_members()
        .await
        .map_err(|e| unreadable("the secret store", e))?;
    let members: std::collections::HashSet<String> = state
        .profile_repo
        .list()
        .await
        .map_err(|e| unreadable("the household", e))?
        .into_iter()
        .map(|profile| profile.id)
        .collect();
    let connected: Vec<String> = connected
        .into_iter()
        .filter(|id| members.contains(id))
        .collect();
    Ok(Json(json!({ "connected": connected })))
}

/// `DELETE /api/v1/uber/accounts/{profile_id}` — forget a member's Uber connection on this pond.
pub async fn disconnect(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    credential: Option<Extension<HostCredential>>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, Refusal> {
    host_only(
        peer,
        credential.as_ref().map(|Extension(c)| c),
        &headers,
        "uber_disconnect",
    )?;
    accounts(&state)?
        .disconnect(&profile_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "uber disconnect: could not update the secret store");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "secret_store_failed",
                "could not forget the Uber connection",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Uber's return to the shared OAuth callback, with the query it came back with: exchange the code
/// through the relay and keep the member's tokens. Records the outcome for the desktop's status
/// poll either way.
pub async fn finish_sign_in(
    state: &AppState,
    session: PkceSession,
    params: &HashMap<String, String>,
    nonce: &str,
) -> Response {
    let code = params.get("code").map_or("", |code| code.trim());
    let outcome = match (session.profile_id.as_deref(), ubers_refusal(params), code) {
        (None, _, _) => Err("this sign-in was not started for a household member".to_string()),
        (_, Some(refusal), _) => Err(refusal),
        (_, None, "") => {
            Err("Uber did not return a sign-in code; the sign-in was cancelled".to_string())
        }
        (Some(profile_id), None, code) => {
            sign_in(state, profile_id, code, &session.redirect_uri).await
        }
    };

    match outcome {
        Ok(()) => {
            oauth_callback::record_outcome(&state.oauth_outcomes, nonce, FlowOutcome::Completed)
                .await;
            Html(
                "<h1>Uber is connected</h1><p>You can close this tab and go back to Goose In A Pond.</p>"
                    .to_string(),
            )
            .into_response()
        }
        Err(reason) => {
            tracing::warn!(reason = %reason, "uber sign-in did not complete");
            oauth_callback::record_outcome(
                &state.oauth_outcomes,
                nonce,
                FlowOutcome::Failed(reason.clone()),
            )
            .await;
            Html(format!(
                "<h1>Uber is not connected</h1><p>{}</p>",
                crate::routes::html_escape(&reason)
            ))
            .into_response()
        }
    }
}

/// Exchange the code, then keep the tokens only for someone still in the household: the exchange
/// takes as long as the relay does, and the member may be removed meanwhile.
/// `redirect_uri` is the one the sign-in was started with, which Uber requires again here.
async fn sign_in(
    state: &AppState,
    profile_id: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<(), String> {
    let accounts = accounts(state).map_err(nothing_kept)?;
    still_a_member(state, profile_id)
        .await
        .map_err(nothing_kept)?;
    let signed_in = accounts
        .exchange(code, redirect_uri)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let _members = members_gate().await;
    still_a_member(state, profile_id)
        .await
        .map_err(nothing_kept)?;
    accounts
        .keep(profile_id, signed_in, chrono::Utc::now())
        .await
        .map_err(|e| format!("{e:#}"))
}

/// A sign-in's failure reason from a refusal met on the way.
fn nothing_kept((_, Json(body)): Refusal) -> String {
    match body["code"].as_str() {
        Some("not_a_member") => {
            "this member is no longer in the household, so nothing was kept".to_string()
        }
        _ => format!(
            "{}; nothing was kept",
            body["error"].as_str().unwrap_or("unavailable")
        ),
    }
}

/// The longest piece of Uber's own reason passed on: it arrives in the address bar.
const REASON_CHARS: usize = 200;

/// Uber's reason when it ended the sign-in itself (`error`, with `error_description` if given).
fn ubers_refusal(params: &HashMap<String, String>) -> Option<String> {
    let present = |name: &str| {
        params
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| v.chars().take(REASON_CHARS).collect::<String>())
    };
    let error = present("error")?;
    Some(match present("error_description") {
        Some(description) => format!("Uber ended the sign-in: {error} ({description})"),
        None => format!("Uber ended the sign-in: {error}"),
    })
}
