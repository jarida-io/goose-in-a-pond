//! A member's profile in My Goose: what the Pond remembers about them, under its headings, and
//! their own hand on it. The policy and the headings are `pond-core`'s `member_profile`.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::models::ports::embedding::EmbeddingProvider as _;
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::member_profile::{
    closed_by, file_under, profile_order, same_sentence, About,
};
use pond_core::user_data::domain::memory::{
    normalise_fact_content, MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment,
};
use pond_core::user_data::domain::profile::ProfileScope;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// Rows a profile reads. More than anyone's profile holds; a bound, not a page.
const PROFILE_ROWS: usize = 400;
/// Archived rows listed for restoring.
const ARCHIVED_ROWS: usize = 100;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/about", get(profile).post(add))
        .route("/me/about/archived", get(archived))
        .route("/me/about/{id}", axum::routing::patch(edit).delete(forget))
        .route("/me/about/{id}/restore", post(restore))
}

fn fact(row: &MemoryFragment, member: &str) -> Value {
    json!({
        "id": row.id,
        "content": row.content,
        "about": row.about.map(About::as_str),
        "sensitive": row.about.is_some_and(About::sensitive),
        // A household row is shown in every member's profile; only its owner's row is theirs.
        "shared": row.profile_id.as_deref() != Some(member),
        "created_at": row.created_at.to_rfc3339(),
        "expires_at": row.valid_until.map(|t| t.to_rfc3339()),
    })
}

/// `GET /me/about`: the calling member's profile, heading by heading.
async fn profile(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let scope = ProfileScope::Owner(member.clone());
    let now = chrono::Utc::now();
    let mut rows: Vec<MemoryFragment> = state
        .memory_repo
        .search_recent(&scope, PROFILE_ROWS)
        .await
        .map_err(server_error)?
        .into_iter()
        .filter(|row| row.is_shown(now))
        .collect();
    profile_order(&mut rows);
    let to_file = rows.iter().filter(|row| row.about.is_none()).count();

    let headings: Vec<Value> = MemorySegment::ALL
        .iter()
        .map(Some)
        .chain([None])
        .filter_map(|segment| {
            let facts: Vec<Value> = rows
                .iter()
                .filter(|row| row.segment.as_ref() == segment)
                .map(|row| fact(row, &member))
                .collect();
            (!facts.is_empty()).then(|| {
                json!({
                    "segment": segment.map_or("other", MemorySegment::as_str),
                    "heading": segment.map_or("Other", MemorySegment::heading),
                    "facts": facts,
                })
            })
        })
        .collect();
    let words: Vec<Value> = About::ALL
        .iter()
        .map(|about| {
            json!({
                "about": about.as_str(),
                "heading": about.segment().heading(),
                "one_at_a_time": about.single_valued(),
            })
        })
        .collect();
    Ok(Json(json!({
        "headings": headings,
        "to_file": to_file,
        "words": words,
    })))
}

#[derive(Deserialize)]
struct AddRequest {
    content: String,
    about: String,
    /// Kept for the whole household rather than as the caller's own.
    #[serde(default)]
    shared: bool,
}

fn about_word(raw: &str) -> Result<About, Failure> {
    About::parse(raw).ok_or_else(|| {
        failure(
            StatusCode::BAD_REQUEST,
            format!("about must be one of: {}", About::listed()),
        )
    })
}

async fn embed(state: &AppState, text: &str) -> Option<Vec<f32>> {
    if !state.embedder.available() {
        return None;
    }
    state.embedder.embed(text).await.ok()
}

/// `POST /me/about`: a fact the member writes about themselves. It is theirs to word, so the
/// sentence is not judged; it is filed under the `about` they pick, and a one-at-a-time fact
/// closes their old one.
async fn add(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(request): Json<AddRequest>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let about = about_word(&request.about)?;
    let content = normalise_fact_content(&request.content);
    if content.is_empty() {
        return Err(failure(StatusCode::BAD_REQUEST, "content is empty"));
    }
    let owner = (!request.shared).then(|| member.clone());
    let scope = ProfileScope::Owner(member.clone());
    let existing: Vec<MemoryFragment> = state
        .memory_repo
        .live_about(&scope, about)
        .await
        .map_err(server_error)?
        .into_iter()
        .filter(|row| row.profile_id == owner)
        .collect();
    if let Some(known) = existing
        .iter()
        .find(|row| same_sentence(&row.content, &content))
    {
        return Ok((StatusCode::OK, Json(fact(known, &member))));
    }

    let mut row = MemoryFragment::from_chat(
        uuid::Uuid::new_v4().to_string(),
        owner,
        None,
        content.clone(),
    );
    row.source = "member".to_string();
    row.embedding = embed(&state, &content).await;
    row.lifecycle = Some(MemoryLifecycle::Active);
    file_under(&mut row, about);
    state
        .memory_repo
        .add(row.clone())
        .await
        .map_err(server_error)?;
    let _ = state
        .memory_repo
        .log_event(MemoryEventKind::Written, &row.id, None, None)
        .await;
    for old in closed_by(about, &content, &existing) {
        if state
            .memory_repo
            .mark_superseded(&old.id, &row.id)
            .await
            .is_ok()
        {
            let _ = state
                .memory_repo
                .log_event(MemoryEventKind::Superseded, &old.id, None, None)
                .await;
        }
    }
    Ok((StatusCode::CREATED, Json(fact(&row, &member))))
}

/// A row the caller may change: their own, never a shared one or another member's.
async fn own_row(state: &AppState, member: &str, id: &str) -> Result<MemoryFragment, Failure> {
    state
        .memory_repo
        .get_in_scope(id, &ProfileScope::Owner(member.to_string()))
        .await
        .map_err(server_error)?
        .filter(|row| row.profile_id.as_deref() == Some(member))
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such memory of yours"))
}

#[derive(Deserialize)]
struct EditRequest {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    about: Option<String>,
}

/// `PATCH /me/about/{id}`: reword one of the member's own facts, or file it under another word.
async fn edit(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    Json(request): Json<EditRequest>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    own_row(&state, &member, &id).await?;
    if let Some(content) = request.content.as_deref() {
        let content = normalise_fact_content(content);
        if content.is_empty() {
            return Err(failure(StatusCode::BAD_REQUEST, "content is empty"));
        }
        let embedding = embed(&state, &content).await;
        state
            .memory_repo
            .update_content(&id, &content, embedding.as_deref())
            .await
            .map_err(server_error)?;
    }
    if let Some(about) = request.about.as_deref() {
        state
            .memory_repo
            .file_row(&id, about_word(about)?)
            .await
            .map_err(server_error)?;
    }
    let row = own_row(&state, &member, &id).await?;
    Ok(Json(fact(&row, &member)))
}

/// `DELETE /me/about/{id}`: forget one of the member's own facts, for good.
async fn forget(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    own_row(&state, &member, &id).await?;
    state.memory_repo.delete(&id).await.map_err(server_error)?;
    let _ = state
        .memory_repo
        .log_event(MemoryEventKind::Deleted, &id, None, None)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Why the re-sort archived a row, from its event's `{"resort": "..."}`.
fn resort_reason(data: Option<&str>) -> Option<String> {
    let value: Value = serde_json::from_str(data?).ok()?;
    value.get("resort")?.as_str().map(str::to_string)
}

/// `GET /me/about/archived`: the member's rows the re-sort set aside, each with why, to restore.
async fn archived(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let rows = state
        .memory_repo
        .archived_of(&member, ARCHIVED_ROWS)
        .await
        .map_err(server_error)?;
    let mut facts = Vec::with_capacity(rows.len());
    for row in &rows {
        let why = state
            .memory_repo
            .get_events(Some(&row.id), 20)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|event| event.event_kind == MemoryEventKind::Archived)
            .find_map(|event| resort_reason(event.data.as_deref()));
        let mut value = fact(row, &member);
        value["why"] = json!(why);
        facts.push(value);
    }
    Ok(Json(json!({ "facts": facts })))
}

/// `POST /me/about/{id}/restore`: bring back a row the re-sort archived.
async fn restore(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let row = own_row(&state, &member, &id).await?;
    if row.lifecycle != Some(MemoryLifecycle::Archived) {
        return Err(failure(StatusCode::CONFLICT, "that memory is not archived"));
    }
    state
        .memory_repo
        .update_lifecycle(&id, MemoryLifecycle::Active)
        .await
        .map_err(server_error)?;
    let _ = state
        .memory_repo
        .log_event(
            MemoryEventKind::Written,
            &id,
            None,
            Some("{\"restored\":true}"),
        )
        .await;
    let row = own_row(&state, &member, &id).await?;
    Ok(Json(fact(&row, &member)))
}
