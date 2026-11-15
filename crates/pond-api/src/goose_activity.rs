//! What Goose did for the calling member by itself: the list they read, taking one back, and
//! marking the list seen. Only the heartbeat writes entries.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::goose_activity::{ActivityKind, GooseActivity};
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::shared::ports::goose_activity::GooseActivityRepository;
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::shared::ports::web_watch::{WatchEdit, WebWatchRepository};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_infra::sqlite_goose_activity::SqliteGooseActivity;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) const SERVED: bool = true;

/// Entries one read returns.
const SHOWN: usize = 60;
/// The member's own mark of how far they have read.
const SEEN_PREFERENCE: &str = "goose_activity_seen_at";

fn repo(state: &AppState) -> SqliteGooseActivity {
    SqliteGooseActivity::new(state.db.system.clone())
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/goose/activity", get(list))
        .route("/me/goose/activity/seen", post(seen))
        .route("/me/goose/activity/{id}/undo", post(undo))
}

fn shown(item: &GooseActivity) -> Value {
    json!({
        "id": item.id,
        "kind": item.kind.as_str(),
        "summary": item.summary,
        "detail": item.detail,
        "at": item.at.to_rfc3339(),
        "can_undo": item.can_undo(),
        "undone": item.undone_at.is_some(),
    })
}

async fn seen_at(state: &AppState, member: &str) -> Result<Option<String>, Failure> {
    let profile = state
        .profile_repo
        .get(member)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such member"))?;
    Ok(profile.preferences.get(SEEN_PREFERENCE).cloned())
}

/// `GET /me/goose/activity`: what Goose did, newest first, and how many entries are new.
async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let items = repo(&state)
        .list_for(&member, SHOWN)
        .await
        .map_err(server_error)?;
    let seen = seen_at(&state, &member).await?;
    let read_to = seen
        .as_deref()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok());
    let unseen = items
        .iter()
        .filter(|item| read_to.is_none_or(|read| item.at > read))
        .count();
    Ok(Json(json!({
        "activity": items.iter().map(shown).collect::<Vec<_>>(),
        "unseen": unseen,
        "seen_at": seen,
    })))
}

/// `POST /me/goose/activity/seen`: the member has read the list down to now.
async fn seen(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let profile = state
        .profile_repo
        .get(&member)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such member"))?;
    let mut preferences = profile.preferences;
    preferences.insert(SEEN_PREFERENCE.to_string(), chrono::Utc::now().to_rfc3339());
    state
        .profile_repo
        .update_preferences(&member, preferences)
        .await
        .map_err(server_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/goose/activity/{id}/undo`: take back what Goose did. The reminder is removed, the
/// note deleted, the watch resumed; what is already gone counts as taken back.
async fn undo(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let repo = repo(&state);
    let item = repo
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such entry"))?;
    let Some(target) = item.ref_id.as_deref().filter(|_| item.can_undo()) else {
        return Err(failure(
            StatusCode::CONFLICT,
            "this cannot be taken back, or already was",
        ));
    };
    let pool = state.db.system.clone();
    match item.kind {
        ActivityKind::Reminder => {
            crate::todos::repo(&state)
                .delete(&member, target)
                .await
                .map_err(server_error)?;
        }
        ActivityKind::Note => {
            pond_infra::sqlite_agent_notes::SqliteAgentNotes::new(pool)
                .remove(target, &ProfileScope::Owner(member.clone()))
                .await
                .map_err(server_error)?;
        }
        ActivityKind::WatchPaused => {
            let resume = WatchEdit {
                looking_for: None,
                filters: None,
                interval_hours: None,
                paused: Some(false),
            };
            pond_infra::sqlite_web_watch::SqliteWebWatchRepository::new(pool)
                .edit(&member, target, &resume)
                .await
                .map_err(server_error)?;
        }
        ActivityKind::NoteClosed
        | ActivityKind::Suggestion
        | ActivityKind::Errand
        | ActivityKind::PhoneRemoved => {}
    }
    repo.mark_undone(&member, &id, chrono::Utc::now())
        .await
        .map_err(server_error)?;
    tracing::info!(
        target: "giap::trace",
        kind = "goose_activity_undone",
        act = item.kind.as_str(),
        "a member took back something Goose did"
    );
    Ok(Json(json!({ "undone": true })))
}
