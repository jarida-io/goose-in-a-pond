//! A member's own notes and their setup interview, reached only from that member's devices.

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::domain::proven_device::ProvenDevice;
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::profile_interview::Interview;
use pond_core::user_data::domain::prompt_extra::{
    check_active_set, PromptExtra, PromptExtraViolation, MAX_ACTIVE_EXTRAS,
    MAX_TOTAL_INSTRUCTION_CHARS,
};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::services::profile_interview::{may_act_for, InterviewService};

use crate::AppState;

type Failure = (StatusCode, Json<Value>);

fn failure(status: StatusCode, error: impl Into<String>) -> Failure {
    (status, Json(json!({"error": error.into()})))
}

fn server_error(e: impl std::fmt::Display) -> Failure {
    failure(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Built per call over the system database; one process-wide lock serialises every instance.
pub(crate) fn interview_service(state: &AppState) -> Option<InterviewService> {
    let extras: Arc<dyn PromptExtraRepository> = state.prompt_extra_repo.clone()?;
    Some(InterviewService::new(
        Arc::new(
            pond_infra::sqlite_profile_interview::SqliteProfileInterviewRepository::new(
                state.db.system.clone(),
            ),
        ),
        extras,
        state.session_storage.clone(),
        state.profile_repo.clone(),
    ))
}

fn extras(state: &AppState) -> Result<Arc<dyn PromptExtraRepository + Send + Sync>, Failure> {
    state.prompt_extra_repo.clone().ok_or_else(|| {
        failure(
            StatusCode::NOT_IMPLEMENTED,
            "Prompt extra repository not configured",
        )
    })
}

/// The household's only member, or `None` for any other count.
pub(crate) async fn sole_member(state: &AppState) -> Option<String> {
    match state.profile_repo.list().await {
        Ok(profiles) if profiles.len() == 1 => profiles.into_iter().next().map(|p| p.id),
        _ => None,
    }
}

/// Refuses unless the caller is `profile_id` (see [`may_act_for`]) and the member exists.
async fn require_member(
    state: &AppState,
    principal: Option<&Extension<Principal>>,
    profile_id: &str,
) -> Result<(), Failure> {
    let device = match principal {
        Some(Extension(principal)) => ProvenDevice::from_principal(principal),
        None => ProvenDevice::none(),
    };
    let rung = match device.id() {
        None => pond_core::security::domain::proven_device::DeviceRung::NoDevice,
        Some(id) => device.rung(
            pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
                state.db.system.clone(),
            )
            .device_profile(id)
            .await,
        ),
    };
    let sole = sole_member(state).await;
    if !may_act_for(&rung, profile_id, sole.as_deref()) {
        tracing::warn!(
            kind = "member_notes_refused",
            profile_id,
            device = device.id().unwrap_or("none"),
            "a caller asked for another member's notes or interview"
        );
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Only this member's own phone can see or change their notes.",
        ));
    }
    match state.profile_repo.get(profile_id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(failure(
            StatusCode::NOT_FOUND,
            format!("no such profile: {profile_id}"),
        )),
        Err(e) => Err(server_error(e)),
    }
}

fn note_json(n: &PromptExtra) -> Value {
    json!({
        "key": n.key,
        "instruction": n.instruction,
        "active": n.active,
        "sort_order": n.sort_order,
    })
}

/// GET /api/v1/profiles/{id}/notes
pub async fn list_notes(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, Failure> {
    require_member(&state, principal.as_ref(), &profile_id).await?;
    let repo = extras(&state)?;
    let notes = repo
        .list_for_profile(&profile_id)
        .await
        .map_err(server_error)?;
    let household = repo.list_active().await.map_err(server_error)?;
    let used_chars: usize = household
        .iter()
        .chain(notes.iter().filter(|n| n.active))
        .map(|n| n.instruction.chars().count())
        .sum();
    Ok(Json(json!({
        "notes": notes.iter().map(note_json).collect::<Vec<_>>(),
        "limits": {
            "max_notes": MAX_ACTIVE_EXTRAS,
            "max_total_chars": MAX_TOTAL_INSTRUCTION_CHARS,
            "used_chars": used_chars,
        },
    })))
}

#[derive(Deserialize)]
pub struct PutNoteRequest {
    instruction: String,
    #[serde(default = "yes")]
    active: bool,
    #[serde(default)]
    sort_order: i32,
}

fn yes() -> bool {
    true
}

fn refused(key: &str, violation: PromptExtraViolation) -> Response {
    tracing::warn!(
        kind = "member_note_refused",
        key = ?key,
        reason = violation.code(),
        "refused a member's note"
    );
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": violation.message(), "code": violation.code()})),
    )
        .into_response()
}

/// PUT /api/v1/profiles/{id}/notes/{key}
pub async fn put_note(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((profile_id, key)): Path<(String, String)>,
    body: Result<Json<PutNoteRequest>, JsonRejection>,
) -> Result<Response, Failure> {
    require_member(&state, principal.as_ref(), &profile_id).await?;
    let Json(req) = body.map_err(|e| failure(StatusCode::BAD_REQUEST, e.to_string()))?;
    let repo = extras(&state)?;
    let note = PromptExtra {
        key,
        instruction: req.instruction,
        active: req.active,
        sort_order: req.sort_order,
        profile_id: Some(profile_id.clone()),
    };
    if let Err(violation) = note.validate() {
        return Ok(refused(&note.key, violation));
    }
    let mut existing = repo.list_active().await.map_err(server_error)?;
    existing.extend(
        repo.list_for_profile(&profile_id)
            .await
            .map_err(server_error)?,
    );
    if let Err(violation) = check_active_set(&existing, &note) {
        return Ok(refused(&note.key, violation));
    }
    repo.upsert(&note).await.map_err(server_error)?;
    tracing::info!(
        kind = "member_note_saved",
        key = %note.key,
        active = note.active,
        instruction_chars = note.instruction.chars().count(),
        "saved a member's note"
    );
    Ok(Json(json!({"key": note.key, "status": "ok"})).into_response())
}

/// DELETE /api/v1/profiles/{id}/notes/{key}
pub async fn delete_note(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((profile_id, key)): Path<(String, String)>,
) -> Result<Json<Value>, Failure> {
    require_member(&state, principal.as_ref(), &profile_id).await?;
    extras(&state)?
        .delete_for_profile(&profile_id, &key)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({"key": key, "status": "deleted"})))
}

/// What the phone shows of an interview: never the answers themselves.
pub(crate) fn interview_json(profile_id: &str, interview: Option<&Interview>) -> Value {
    let Some(i) = interview else {
        return json!({"profile_id": profile_id, "status": "not_started"});
    };
    json!({
        "profile_id": profile_id,
        "status": i.status.as_str(),
        "session_id": i.session_id,
        "topics": i.topics.iter().map(|t| json!({
            "id": t.topic.id(),
            "label": t.topic.label(),
            "status": t.status,
        })).collect::<Vec<_>>(),
        "draft": i.draft.iter().map(|d| json!({
            "key": d.topic.note_key(),
            "topic": d.topic.id(),
            "instruction": d.instruction,
        })).collect::<Vec<_>>(),
    })
}

fn service(state: &AppState) -> Result<InterviewService, Failure> {
    interview_service(state).ok_or_else(|| {
        failure(
            StatusCode::NOT_IMPLEMENTED,
            "Prompt extra repository not configured",
        )
    })
}

/// GET /api/v1/profiles/{id}/interview
pub async fn interview_status(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, Failure> {
    require_member(&state, principal.as_ref(), &profile_id).await?;
    let interview = service(&state)?
        .get(&profile_id)
        .await
        .map_err(server_error)?;
    Ok(Json(interview_json(&profile_id, interview.as_ref())))
}

/// POST /api/v1/profiles/{id}/interview: starts it, or takes up the open or stopped one.
pub async fn start_interview(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, Failure> {
    require_member(&state, principal.as_ref(), &profile_id).await?;
    let interview = service(&state)?
        .start(&profile_id, Utc::now())
        .await
        .map_err(server_error)?;
    Ok(Json(interview_json(&profile_id, Some(&interview))))
}

/// POST /api/v1/profiles/{id}/interview/approve: the member tapped Approve on the draft.
pub async fn approve_interview(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, Failure> {
    require_member(&state, principal.as_ref(), &profile_id).await?;
    match service(&state)?
        .approve(&profile_id, true, Utc::now())
        .await
        .map_err(server_error)?
    {
        Ok(saved) => Ok(Json(json!({"saved": saved}))),
        Err(refusal) => Err((
            StatusCode::CONFLICT,
            Json(json!({"error": refusal.message(), "code": refusal.code()})),
        )),
    }
}
