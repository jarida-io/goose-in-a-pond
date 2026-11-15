//! `/me/screenshots`: ideas from the member's phone screenshots, off until they turn it on.
//!
//! The phone sends a screenshot; the Pond reads it once on the lane for a one-line note and
//! drops the picture, which is held in memory and never written to disk. The member sees every
//! note and can delete any of them, or all.

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

use pond_core::models::domain::message::ImageAttachment;
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::ports::screenshot_notes::ScreenshotNoteRepository;
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_core::user_data::services::screenshot_notes::{
    self as shots, NotTaken, Shot, MAX_PENDING, PREFERENCE,
};
use pond_infra::sqlite_screenshot_notes::SqliteScreenshotNoteRepository;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/me/screenshots",
            get(read).put(choose).post(send).delete(forget_all),
        )
        .route("/me/screenshots/{id}", delete(forget))
}

fn repo(state: &AppState) -> SqliteScreenshotNoteRepository {
    SqliteScreenshotNoteRepository::new(state.db.system.clone())
}

/// Whether the member turned it on.
async fn is_on(state: &AppState, member: &str) -> Result<bool, Failure> {
    let profile = state
        .profile_repo
        .get(member)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such member"))?;
    Ok(shots::is_on(&profile.preferences))
}

/// Whether this Pond's model can look at a picture at all.
async fn can_read_pictures(state: &AppState) -> bool {
    state
        .llm_provider
        .read()
        .await
        .as_ref()
        .is_some_and(|provider| provider.capabilities().vision)
}

async fn view(state: &AppState, member: &str) -> Result<Value, Failure> {
    let notes = repo(state).list(member).await.map_err(server_error)?;
    Ok(json!({
        "enabled": is_on(state, member).await?,
        "available": can_read_pictures(state).await,
        "pending": shots::pending().count_for(member),
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
    let profile = state
        .profile_repo
        .get(&member)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such member"))?;
    let mut preferences = profile.preferences;
    preferences.insert(PREFERENCE.to_string(), choice.enabled.to_string());
    state
        .profile_repo
        .update_preferences(&member, preferences)
        .await
        .map_err(server_error)?;
    if !choice.enabled {
        shots::pending().drop_for(&member);
    }
    tracing::info!(
        target: "giap::trace",
        kind = "screenshots_choice",
        enabled = choice.enabled,
        "a member chose whether their screenshots feed their ideas"
    );
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

/// `POST /me/screenshots`: one screenshot to read. `409` unless the member turned this on and
/// the model reads pictures; `413` too large; `429` when enough are already waiting.
async fn send(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<Sent>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(sent) = body.map_err(|e| failure(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()))?;
    if !is_on(&state, &member).await? {
        return Err(failure(
            StatusCode::CONFLICT,
            "ideas from screenshots are off; turn them on first",
        ));
    }
    if !can_read_pictures(&state).await {
        return Err(failure(
            StatusCode::CONFLICT,
            "this Pond's model cannot read pictures",
        ));
    }
    let shot = Shot::new(
        member.clone(),
        ImageAttachment {
            data: sent.image,
            mime_type: sent.mime_type.trim().to_lowercase(),
        },
        sent.taken_at,
    );
    let pending = shots::pending().push(shot).map_err(|refusal| {
        let status = match refusal {
            NotTaken::QueueFull => StatusCode::TOO_MANY_REQUESTS,
            NotTaken::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            NotTaken::WrongType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        };
        failure(status, refusal.as_str())
    })?;
    if let Some(lane) = &state.lane {
        let _ = lane.wake(LaneJob::ScreenshotNotes).await;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "pending": pending }))))
}

async fn forget(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match repo(&state).delete(&member, &id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(failure(StatusCode::NOT_FOUND, "no such note of yours")),
        Err(e) => Err(server_error(e)),
    }
}

/// `DELETE /me/screenshots`: every note, and every screenshot still waiting.
async fn forget_all(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let waiting = shots::pending().drop_for(&member);
    let deleted = repo(&state)
        .delete_all(&member)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({ "deleted": deleted, "dropped": waiting })))
}
