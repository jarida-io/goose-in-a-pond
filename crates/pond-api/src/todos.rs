//! A member's to-dos and reminders, the hooks that fire them, the places their phone fences, and
//! the dated mentions from their chats offered as to-dos. Goose's check-ins and hook firings run
//! in `pond-server`'s hooks runner; these routes only read and write the member's rows.

mod sources;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::todo::{
    apply_patch, hook_drafts, new_todo, zone, Answer, AnswerRefused, HookCatalog, HookError,
    HookEvent, Place, StateFilter, Todo, TodoSource, Transition, Tz, MAX_PLACES_PER_MEMBER,
};
use pond_core::shared::ports::todo::{HookEventQueue, TodoRepository};
use pond_core::shared::services::todo_offers::{offer_due, offer_view, todo_from_offer};
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use pond_infra::sqlite_reminder::SqliteReminderRepository;
use pond_infra::sqlite_todo::SqliteTodoRepository;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) use sources::hook_sources;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// A due time this far in the past is still taken as meant: the phone's clock and the Pond's
/// differ by seconds.
const CLOCK_SLACK_SECS: i64 = 60;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/todos", get(list).post(create))
        .route("/me/todos/offers", get(offers))
        .route("/me/todos/offers/{reminder_id}/accept", post(accept_offer))
        .route(
            "/me/todos/offers/{reminder_id}/decline",
            post(decline_offer),
        )
        .route("/me/todos/{id}", patch(edit).delete(remove))
        .route("/me/todos/{id}/answer", post(answer))
        .route("/me/hooks/sources", get(sources::list))
        .route("/me/places", post(add_place))
        .route("/me/places/{id}", axum::routing::delete(remove_place))
        .route("/me/places/{id}/event", post(place_event))
}

pub(crate) fn repo(state: &AppState) -> SqliteTodoRepository {
    SqliteTodoRepository::new(state.db.system.clone())
}

/// The member's timezone: the household's, which every member shares today.
pub(crate) async fn timezone(state: &AppState) -> Tz {
    state
        .settings_repo
        .get()
        .await
        .map_or_else(|_| zone(""), |s| zone(&s.timezone))
}

fn refused(HookError(why): HookError) -> Failure {
    failure(StatusCode::BAD_REQUEST, why)
}

fn not_found(what: &str) -> Failure {
    failure(StatusCode::NOT_FOUND, format!("no such {what}"))
}

/// A JSON body, or a 400 naming what is wrong with it; an empty body reads as `{}`.
fn body_of(bytes: &Bytes) -> Result<Value, Failure> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice(bytes).map_err(|e| {
        failure(
            StatusCode::BAD_REQUEST,
            format!("the body is not JSON: {e}"),
        )
    })
}

/// What a body's hooks may name, read only when it sends hooks.
async fn catalog_for(state: &AppState, member: &str, body: &Value) -> Result<HookCatalog, Failure> {
    match hook_drafts(body).map_err(refused)? {
        Some(drafts) if !drafts.is_empty() => hook_sources(state, member).await,
        _ => Ok(HookCatalog::default()),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListQuery {
    state: Option<String>,
}

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Some(filter) = StateFilter::parse(query.state.as_deref()) else {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "`state` is open, done, dropped or all",
        ));
    };
    let todos = repo(&state)
        .list(&member, filter)
        .await
        .map_err(server_error)?;
    Ok(Json(
        json!({"todos": todos.iter().map(Todo::view).collect::<Vec<_>>()}),
    ))
}

/// Creates a to-do for `member` from a `POST /me/todos` body; also how an idea's steps or Goose
/// add one with their own `source`.
pub(crate) async fn add(
    state: &AppState,
    member: &str,
    body: &Value,
    source: TodoSource,
    source_ref: Option<String>,
) -> Result<Todo, Failure> {
    let catalog = catalog_for(state, member, body).await?;
    let todo = new_todo(
        member,
        body,
        &catalog,
        source,
        source_ref,
        Utc::now(),
        timezone(state).await,
    )
    .map_err(refused)?;
    repo(state).create(&todo).await.map_err(server_error)?;
    Ok(todo)
}

async fn create(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Bytes,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = body_of(&body)?;
    let todo = add(&state, &member, &body, TodoSource::You, None).await?;
    Ok((StatusCode::CREATED, Json(todo.view())).into_response())
}

async fn edit(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = body_of(&body)?;
    let repo = repo(&state);
    let Some(mut todo) = repo.get(&member, &id).await.map_err(server_error)? else {
        return Err(not_found("to-do"));
    };
    let catalog = catalog_for(&state, &member, &body).await?;
    let replace_hooks = apply_patch(
        &mut todo,
        &body,
        &catalog,
        Utc::now(),
        timezone(&state).await,
    )
    .map_err(refused)?;
    repo.update(&todo, replace_hooks)
        .await
        .map_err(server_error)?;
    Ok(Json(todo.view()))
}

async fn remove(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match repo(&state).delete(&member, &id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(not_found("to-do")),
        Err(e) => Err(server_error(e)),
    }
}

async fn answer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = body_of(&body)?;
    let Some(answer) = body
        .get("answer")
        .and_then(Value::as_str)
        .and_then(Answer::parse)
    else {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "`answer` is done, not_yet or drop",
        ));
    };
    let snooze = match body.get("snooze_minutes") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64().and_then(|m| u32::try_from(m).ok()) {
            Some(m) => Some(m),
            None => {
                return Err(failure(
                    StatusCode::BAD_REQUEST,
                    "`snooze_minutes` must be a whole number of minutes",
                ))
            }
        },
    };
    let repo = repo(&state);
    let Some(mut todo) = repo.get(&member, &id).await.map_err(server_error)? else {
        return Err(not_found("to-do"));
    };
    match todo.answer(answer, snooze, Utc::now(), timezone(&state).await) {
        Ok(()) => {}
        Err(AnswerRefused::Closed(closed)) => {
            return Err(failure(
                StatusCode::CONFLICT,
                format!("this to-do is already {}", closed.as_str()),
            ))
        }
        Err(AnswerRefused::Snooze) => {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "`snooze_minutes` runs from 1 to 10080, a week",
            ))
        }
    }
    repo.update(&todo, false).await.map_err(server_error)?;
    Ok(Json(todo.view()))
}

async fn add_place(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Bytes,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = body_of(&body)?;
    let number = |key: &str| body.get(key).and_then(Value::as_f64);
    let (Some(label), Some(lat), Some(lng)) = (
        body.get("label").and_then(Value::as_str),
        number("lat"),
        number("lng"),
    ) else {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "a place needs `label`, `lat` and `lng`",
        ));
    };
    let radius = match body.get("radius_m") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_f64()
                .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "`radius_m` must be a number"))?,
        ),
    };
    let place = Place::new(&member, label, lat, lng, radius, Utc::now()).map_err(refused)?;
    let repo = repo(&state);
    if repo.places(&member).await.map_err(server_error)?.len() >= MAX_PLACES_PER_MEMBER {
        return Err(failure(
            StatusCode::CONFLICT,
            format!("a member keeps at most {MAX_PLACES_PER_MEMBER} places"),
        ));
    }
    repo.add_place(&place).await.map_err(server_error)?;
    Ok((StatusCode::CREATED, Json(place.view())).into_response())
}

async fn remove_place(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match repo(&state).delete_place(&member, &id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(not_found("place")),
        Err(e) => Err(server_error(e)),
    }
}

/// The phone's geofence crossing one of the member's places, queued for their hooks.
async fn place_event(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = body_of(&body)?;
    let Some(transition) = body
        .get("transition")
        .and_then(Value::as_str)
        .and_then(Transition::parse)
    else {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "`transition` is arrive or leave",
        ));
    };
    let repo = repo(&state);
    let places = repo.places(&member).await.map_err(server_error)?;
    let Some(place) = places.iter().find(|p| p.id == id) else {
        return Err(not_found("place"));
    };
    repo.raise(&HookEvent::new(
        Some(&member),
        "place",
        &place.id,
        transition.as_str(),
        json!({"label": place.label}),
        Utc::now(),
    ))
    .await
    .map_err(server_error)?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))).into_response())
}

fn reminders(state: &AppState) -> SqliteReminderRepository {
    SqliteReminderRepository::new(state.db.system.clone())
}

async fn offers(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let (now, tz) = (Utc::now(), timezone(&state).await);
    let offered = reminders(&state)
        .open_to(&member)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({
        "offers": offered
            .iter()
            .map(|r| offer_view(r, offer_due(r, now, tz)))
            .collect::<Vec<_>>(),
    })))
}

async fn accept_offer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(reminder_id): Path<String>,
    body: Bytes,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = body_of(&body)?;
    let (now, tz) = (Utc::now(), timezone(&state).await);
    let reminders = reminders(&state);
    let offered = reminders.open_to(&member).await.map_err(server_error)?;
    let Some(reminder) = offered.iter().find(|r| r.id == reminder_id) else {
        return Err(not_found("offer"));
    };
    let sent = match body.get("due_at") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                .map(|t| t.with_timezone(&Utc))
                .ok_or_else(|| {
                    failure(StatusCode::BAD_REQUEST, "`due_at` must be an RFC 3339 time")
                })?,
        ),
    };
    if sent.is_some_and(|due| due < now - Duration::seconds(CLOCK_SLACK_SECS)) {
        return Err(failure(StatusCode::BAD_REQUEST, "`due_at` has passed"));
    }
    let Some(due) = sent.or_else(|| offer_due(reminder, now, tz)) else {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "due_at_required: Goose could not work out a time from what was said; send `due_at`",
        ));
    };
    let todo = todo_from_offer(reminder, &member, due, now, tz);
    let repo = repo(&state);
    repo.create(&todo).await.map_err(server_error)?;
    match reminders
        .settle_offer(&member, &reminder_id, true, now)
        .await
    {
        Ok(true) => Ok((StatusCode::CREATED, Json(todo.view())).into_response()),
        // Answered from another screen in between: that answer stands.
        Ok(false) => {
            let _ = repo.delete(&member, &todo.id).await;
            Err(not_found("offer"))
        }
        Err(e) => {
            let _ = repo.delete(&member, &todo.id).await;
            Err(server_error(e))
        }
    }
}

async fn decline_offer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(reminder_id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match reminders(&state)
        .settle_offer(&member, &reminder_id, false, Utc::now())
        .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(not_found("offer")),
        Err(e) => Err(server_error(e)),
    }
}
