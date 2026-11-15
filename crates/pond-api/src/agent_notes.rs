//! Goose's own notes as the calling member may see them: the lessons it learnt working for
//! them and the follow-ups it means to come back to. A member reads and removes their own and
//! the household's; they do not write them, Goose does (`note_to_self`, the heartbeat).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Extension, Json, Router,
};
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::agent_note::{AgentNote, NoteKind};
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::user_data::domain::profile::ProfileScope;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

fn repo(state: &AppState) -> Arc<dyn AgentNotesRepository> {
    Arc::new(pond_infra::sqlite_agent_notes::SqliteAgentNotes::new(
        state.db.system.clone(),
    ))
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/notes", get(list))
        .route("/me/notes/{id}", delete(remove))
}

fn shown(note: &AgentNote, member: &str) -> Value {
    json!({
        "id": note.id,
        "text": note.text,
        "created_at": note.created_at.to_rfc3339(),
        // The household's own, shown to every member.
        "shared": note.profile_id.as_deref() != Some(member),
    })
}

/// `GET /me/notes`: what Goose has written down for itself about working for this member.
async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let scope = ProfileScope::Owner(member.clone());
    let repo = repo(&state);
    let of = |notes: Vec<AgentNote>| -> Vec<Value> {
        notes.iter().map(|note| shown(note, &member)).collect()
    };
    let lessons = repo
        .list(&scope, NoteKind::Lesson)
        .await
        .map_err(server_error)?;
    let notes = repo
        .list(&scope, NoteKind::Note)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({
        "lessons": of(lessons),
        "notes": of(notes),
    })))
}

/// `DELETE /me/notes/{id}`: take a lesson or a follow-up away from Goose.
async fn remove(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let removed = repo(&state)
        .remove(&id, &ProfileScope::Owner(member))
        .await
        .map_err(server_error)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(failure(StatusCode::NOT_FOUND, "no such note"))
    }
}
