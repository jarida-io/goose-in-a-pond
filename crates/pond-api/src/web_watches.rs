//! A member's web watches for their screens: the list, one watch with its changes, an edit (what
//! they look for, how often, paused) and a delete, and whether their matches are also said aloud.
//! Each route reaches only the caller's own watches; a watch is started from a conversation, where
//! the page is read first.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use pond_core::security::ports::policy::Principal;
use pond_core::security::ports::remote_access::TailnetMachine;
use pond_core::shared::domain::web_watch::{WatchFilters, MAX_INTERVAL_HOURS, MIN_INTERVAL_HOURS};
use pond_core::shared::ports::web_watch::{StoredChange, WatchEdit, WebWatch, WebWatchRepository};
use pond_core::shared::services::spoken_watch_alerts::{is_time_of_day, SpokenAlertChoice};
use pond_core::user_data::domain::settings::Settings;
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
        // For a rule read off the whole page: how the page stood at the last check.
        "standing": watch.filters.tell_when.standing(items),
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
    /// As sent, so a screen that does not know rules is told apart from one clearing a rule.
    pub filters: Option<Value>,
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
    let repo = repo(&state);
    let filters = match body.filters {
        None => None,
        Some(sent) => {
            let names_rule = sent.get("tell_when").is_some();
            let mut filters: WatchFilters = match serde_json::from_value(sent) {
                Ok(filters) => filters,
                Err(e) => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({"error": format!("filters: {e}")})),
                    )
                        .into_response()
                }
            };
            // An edit from a screen that sends no rule keeps the one the watch has.
            if !names_rule {
                match repo.get(&id).await {
                    Ok(Some(watch)) if watch.owner == member => {
                        filters.tell_when = watch.filters.tell_when;
                    }
                    Ok(_) => return not_found(),
                    Err(e) => return failed(e),
                }
            }
            Some(filters)
        }
    };
    let edit = WatchEdit {
        looking_for: body.looking_for.map(|t| t.trim().to_string()),
        filters,
        interval_hours: body
            .interval_hours
            .map(|h| h.clamp(MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS)),
        paused: body.paused,
    };
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

#[derive(Debug, Default, Deserialize)]
pub struct SpokenAlertsBody {
    pub enabled: Option<bool>,
    pub quiet_start: Option<String>,
    pub quiet_end: Option<String>,
}

/// The member's choice, beside the household's own switch and quiet hours, which also apply.
fn spoken_view(choice: &SpokenAlertChoice, household: Option<&Settings>) -> Value {
    json!({
        "enabled": choice.enabled,
        "quiet_start": choice.quiet_start,
        "quiet_end": choice.quiet_end,
        "household_allows_speech": household.map(|s| s.unprompted_speech_enabled),
        "household_quiet_start": household.map(|s| s.quiet_hours_start.as_str()),
        "household_quiet_end": household.map(|s| s.quiet_hours_end.as_str()),
    })
}

async fn spoken_choice(
    state: &Arc<AppState>,
    member: &str,
) -> Result<(HashMap<String, String>, SpokenAlertChoice), Response> {
    match state.profile_repo.get(member).await {
        Ok(Some(profile)) => {
            let choice = SpokenAlertChoice::of(&profile.preferences);
            Ok((profile.preferences, choice))
        }
        Ok(None) => Err(unknown_caller()),
        Err(e) => Err(failed(e)),
    }
}

/// `GET /api/v1/web-watches/spoken-alerts` — whether the caller's matches are said aloud.
pub async fn spoken_alerts(
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
        return unknown_caller();
    };
    let (_, choice) = match spoken_choice(&state, &member).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let household = state.settings_repo.get().await.ok();
    Json(spoken_view(&choice, household.as_ref())).into_response()
}

/// `PUT /api/v1/web-watches/spoken-alerts` — turn spoken matches on or off, or move the
/// caller's quiet hours. Fields left out keep their value.
pub async fn set_spoken_alerts(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Json(body): Json<SpokenAlertsBody>,
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
    for time in [&body.quiet_start, &body.quiet_end].into_iter().flatten() {
        if !is_time_of_day(time) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "quiet hours are a time of day such as 22:00"})),
            )
                .into_response();
        }
    }
    let (mut preferences, mut choice) = match spoken_choice(&state, &member).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    if let Some(enabled) = body.enabled {
        choice.enabled = enabled;
    }
    if let Some(start) = body.quiet_start {
        choice.quiet_start = start.trim().to_string();
    }
    if let Some(end) = body.quiet_end {
        choice.quiet_end = end.trim().to_string();
    }
    choice.write_into(&mut preferences);
    if let Err(e) = state
        .profile_repo
        .update_preferences(&member, preferences)
        .await
    {
        return failed(e);
    }
    let household = state.settings_repo.get().await.ok();
    Json(spoken_view(&choice, household.as_ref())).into_response()
}
