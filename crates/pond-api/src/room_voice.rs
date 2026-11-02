//! `/api/v1/voice/room`: serve's room voice loop, its switch, its events and its interrupt.

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use pond_core::shared::domain::agent::WorkflowEvent;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

const SETTING: &str = "voice_room_enabled";

fn no_room() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "this pond runs no room voice",
            "reason": "room_voice_unavailable",
        })),
    )
        .into_response()
}

/// `GET /api/v1/voice/room`
pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    match &state.room_voice {
        Some(room) => Json(room.status()).into_response(),
        None => no_room(),
    }
}

#[derive(Deserialize)]
pub struct Enable {
    enabled: bool,
}

/// `PUT /api/v1/voice/room {enabled}`: saved as the household's choice, then applied at once.
pub async fn set_enabled(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Enable>, JsonRejection>,
) -> Response {
    let Some(room) = state.room_voice.clone() else {
        return no_room();
    };
    let Ok(Json(Enable { enabled })) = body else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "expected {\"enabled\": true|false}"})),
        )
            .into_response();
    };
    if let Err(e) = state
        .settings_repo
        .set_key(SETTING, enabled.to_string())
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("could not save the room setting: {e}")})),
        )
            .into_response();
    }
    let keys: HashSet<String> = [SETTING.to_string()].into();
    if let Err(e) = state.settings_repo.mark_user_set(&keys).await {
        tracing::warn!(error = %e, "room voice: could not record the choice as the household's");
    }
    Json(room.set_enabled(enabled).await).into_response()
}

#[derive(Deserialize, Default)]
pub struct EventsQuery {
    /// `1` to receive the microphone and speaker levels too.
    #[serde(default)]
    levels: Option<String>,
}

/// `GET /api/v1/voice/room/events`: each `data:` is one NDJSON line of the room's loop.
pub async fn events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
) -> Response {
    let Some(room) = state.room_voice.clone() else {
        return no_room();
    };
    let levels = query.levels.as_deref() == Some("1");
    Sse::new(event_stream(room.subscribe(), levels))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn event_stream(
    mut events: tokio::sync::broadcast::Receiver<WorkflowEvent>,
    levels: bool,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        loop {
            match events.recv().await {
                Ok(WorkflowEvent::AudioLevel { .. }) if !levels => {}
                Ok(event) => {
                    if let Some(line) = event.to_ndjson() {
                        yield Ok(Event::default().data(line));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "room voice events: a slow client missed some");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

/// `POST /api/v1/voice/room/interrupt`: ends the turn in flight, as the shell's button does.
pub async fn interrupt(State(state): State<Arc<AppState>>) -> Response {
    match &state.room_voice {
        Some(room) => {
            room.interrupt();
            StatusCode::NO_CONTENT.into_response()
        }
        None => no_room(),
    }
}

#[derive(Deserialize)]
pub struct Pause {
    seconds: u64,
}

/// `POST /api/v1/voice/room/pause {seconds}`: no wake word and no new turn for up to five
/// minutes, as while a member teaches the pond their voice; never saved, and it resumes on its own.
pub async fn pause(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Pause>, JsonRejection>,
) -> Response {
    use pond_core::models::domain::room_voice::MAX_ROOM_PAUSE_SECONDS;
    let Some(room) = state.room_voice.clone() else {
        return no_room();
    };
    let seconds = match body {
        Ok(Json(Pause { seconds })) if (1..=MAX_ROOM_PAUSE_SECONDS).contains(&seconds) => seconds,
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": format!("expected {{\"seconds\": 1 to {MAX_ROOM_PAUSE_SECONDS}}}"),
                })),
            )
                .into_response()
        }
    };
    room.pause(std::time::Duration::from_secs(seconds));
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v1/voice/room/resume`
pub async fn resume(State(state): State<Arc<AppState>>) -> Response {
    match &state.room_voice {
        Some(room) => {
            room.resume();
            StatusCode::NO_CONTENT.into_response()
        }
        None => no_room(),
    }
}
