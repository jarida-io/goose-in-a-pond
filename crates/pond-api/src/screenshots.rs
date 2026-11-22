//! `/me/screenshots`: the routes a phone build without captures calls, in the shapes it reads,
//! served from the member's captures of kind `screenshot`. A note is a read capture's
//! description, and `enabled` is the member's screenshots setting.

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{delete, get},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::capture::{Capture, CaptureKind, CaptureState};

use crate::captures::{self, SettingsChange, MAX_PENDING};
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Notes one read lists, newest first: as many as that build's screen was made to hold.
const NOTES_LISTED: usize = 40;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/me/screenshots",
            get(read).put(choose).post(send).delete(forget_all),
        )
        .route("/me/screenshots/{id}", delete(forget))
}

fn note(capture: &Capture) -> Option<Value> {
    let note = capture.description.as_deref()?;
    Some(json!({
        "id": capture.id,
        "note": note,
        "taken_at": capture.taken_at,
        "created_at": capture.created_at,
        "used_at": null,
    }))
}

async fn view(state: &AppState, member: &str) -> Result<Value, Failure> {
    let settings = captures::settings_of(state, member).await?;
    let shots: Vec<Capture> = state
        .engine
        .captures
        .list(member)
        .await
        .map_err(server_error)?
        .into_iter()
        .filter(|capture| capture.kind == CaptureKind::Screenshot)
        .collect();
    let pending = shots
        .iter()
        .filter(|shot| shot.state == CaptureState::Pending)
        .count();
    let notes: Vec<Value> = shots
        .iter()
        .filter(|shot| captures::is_shown(shot))
        .filter_map(note)
        .take(NOTES_LISTED)
        .collect();
    Ok(json!({
        "enabled": settings.screenshots,
        "available": captures::can_read_pictures(state).await,
        "pending": pending,
        "max_pending": MAX_PENDING,
        "notes": notes,
    }))
}

async fn read(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    Ok(Json(view(&state, &member).await?))
}

#[derive(Debug, Deserialize)]
struct Choice {
    enabled: bool,
}

/// `PUT /me/screenshots` `{"enabled": bool}`. Turning it off forgets every screenshot still
/// waiting to be read; the notes already written stay until the member deletes them.
async fn choose(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<Choice>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(choice) =
        body.map_err(|e| failure(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()))?;
    let change = SettingsChange {
        screenshots: Some(choice.enabled),
        albums: None,
    };
    captures::change_settings(&state, &member, change).await?;
    Ok(Json(view(&state, &member).await?))
}

#[derive(Debug, Deserialize)]
struct Sent {
    /// Base64 of a JPEG, PNG or WebP.
    image: String,
    mime_type: String,
    #[serde(default)]
    taken_at: Option<DateTime<Utc>>,
}

/// `POST /me/screenshots`: one screenshot to read, refused as `POST /me/captures` refuses one
/// and answered `202` whether or not the member sent it before.
async fn send(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<Sent>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(sent) = body.map_err(captures::bad_picture_body)?;
    let sent = captures::Sent {
        image: sent.image,
        mime_type: sent.mime_type,
        kind: CaptureKind::Screenshot,
        album: None,
        taken_at: sent.taken_at,
    };
    let pending = match captures::take(&state, &member, sent).await? {
        captures::Taken::Queued { pending } | captures::Taken::Held { pending, .. } => pending,
    };
    Ok((StatusCode::ACCEPTED, Json(json!({ "pending": pending }))))
}

async fn forget(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let shot = state
        .engine
        .captures
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .filter(|capture| capture.kind == CaptureKind::Screenshot)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such note of yours"))?;
    captures::forget(&state, &member, std::slice::from_ref(&shot)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /me/screenshots`: every screenshot the member sent, read or still waiting.
async fn forget_all(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let shots: Vec<Capture> = captures::every_capture(&state, &member)
        .await?
        .into_iter()
        .filter(|capture| capture.kind == CaptureKind::Screenshot)
        .collect();
    let waiting = shots
        .iter()
        .filter(|shot| shot.state == CaptureState::Pending)
        .count();
    captures::forget(&state, &member, &shots).await?;
    Ok(Json(json!({
        "deleted": shots.len() - waiting,
        "dropped": waiting,
    })))
}
