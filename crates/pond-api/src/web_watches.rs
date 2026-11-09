//! A member's web watches for their screens: the list, one watch with its changes, an edit (what
//! they look for, how often, paused) and a delete. Each route reaches only the caller's own
//! watches; a watch is started from a conversation, where the page is read first.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use pond_core::security::ports::policy::Principal;
use pond_core::security::ports::remote_access::TailnetMachine;
use pond_core::shared::domain::web_watch::{WatchFilters, MAX_INTERVAL_HOURS, MIN_INTERVAL_HOURS};
use pond_core::shared::ports::web_watch::{StoredChange, WatchEdit, WebWatch, WebWatchRepository};
use pond_infra::sqlite_web_watch::SqliteWebWatchRepository;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

/// Changes one watch's view lists, newest first.
const CHANGES_SHOWN: usize = 50;

type Peer = Result<
    axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::rejection::ExtensionRejection,
>;
type Insecure = Option<Extension<crate::insecure_dev::InsecureDevTransport>>;

fn repo(state: &Arc<AppState>) -> SqliteWebWatchRepository {
    SqliteWebWatchRepository::new(state.db.system.clone())
}

async fn caller(
    state: &Arc<AppState>,
    principal: Option<&Extension<Principal>>,
    peer: Peer,
    insecure: bool,
    tailnet: bool,
) -> Option<String> {
    crate::routes::long_task_caller(state, principal, peer, insecure, tailnet).await
}

fn failed(e: anyhow::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "no such watch"})),
    )
        .into_response()
}

fn unknown_caller() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "watches belong to a household member; this device is not one"})),
    )
        .into_response()
}

/// One watch as its screen shows it.
pub fn watch_view(watch: &WebWatch) -> Value {
    let items = watch.items.as_deref().unwrap_or_default();
    json!({
        "id": watch.id,
        "title": watch.title,
        "address": watch.address,
        "looking_for": watch.looking_for,
        "filters": watch.filters,
        "filters_words": watch.filters.words_for_people(),
        "interval_hours": watch.interval_hours,
        "paused": watch.paused,
        "items_count": items.len(),
        "matching_count": items.iter().filter(|i| watch.filters.matches(i)).count(),
        "failures": watch.failures,
        "last_checked_at": watch.last_checked_at,
        "next_check_at": watch.next_check_at,
        "created_at": watch.created_at,
        "session_id": watch.session_id,
    })
}

fn change_view(change: &StoredChange) -> Value {
    let mut view = json!({
        "id": change.id,
        "found_at": change.found_at,
        "line": change.change.line(),
        "matched": change.change.matched,
        "item": change.change.item,
    });
    if let (Value::Object(view), Ok(Value::Object(kind))) =
        (&mut view, serde_json::to_value(&change.change.kind))
    {
        view.extend(kind);
    }
    view
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return Json(json!([])).into_response();
    };
    match repo(&state).list_for(&member).await {
        Ok(watches) => Json(Value::Array(watches.iter().map(watch_view).collect())).into_response(),
        Err(e) => failed(e),
    }
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    let repo = repo(&state);
    let watch = match repo.get(&id).await {
        Ok(Some(watch)) if watch.owner == member => watch,
        Ok(_) => return not_found(),
        Err(e) => return failed(e),
    };
    let changes = match repo.changes(&id, CHANGES_SHOWN).await {
        Ok(changes) => changes,
        Err(e) => return failed(e),
    };
    let mut view = watch_view(&watch);
    view["items"] = json!(watch.items.unwrap_or_default());
    view["changes"] = Value::Array(changes.iter().map(change_view).collect());
    Json(view).into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct WatchEditBody {
    pub looking_for: Option<String>,
    pub filters: Option<WatchFilters>,
    pub interval_hours: Option<u32>,
    pub paused: Option<bool>,
}

pub async fn edit(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
    Json(body): Json<WatchEditBody>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    let edit = WatchEdit {
        looking_for: body.looking_for.map(|t| t.trim().to_string()),
        filters: body.filters,
        interval_hours: body
            .interval_hours
            .map(|h| h.clamp(MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS)),
        paused: body.paused,
    };
    let repo = repo(&state);
    match repo.edit(&member, &id, &edit).await {
        Ok(true) => match repo.get(&id).await {
            Ok(Some(watch)) => Json(watch_view(&watch)).into_response(),
            Ok(None) => not_found(),
            Err(e) => failed(e),
        },
        Ok(false) => not_found(),
        Err(e) => failed(e),
    }
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    match repo(&state).delete(&member, &id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(),
        Err(e) => failed(e),
    }
}
