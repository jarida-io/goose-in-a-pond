//! What a member is into, the interview that asks them, and the Pond's portrait of them.
//!
//! An interview turn is one direct model call, not a lane job: the member is waiting for the
//! next question. The route sets the interview `thinking`, spawns the call and answers at once;
//! the phone polls `GET` until the question is there.

use std::sync::{Arc, LazyLock};

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::models::domain::message::ChatMessage;
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::interest::{InterestInput, InterestPatch, Origin, OFFERED_AT};
use pond_core::user_data::domain::interest_interview::{InterestInterview, InterviewStatus};
use pond_core::user_data::domain::memory::MemorySegment;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::services::interest_interview::{self as interview, KnownAboutMember};

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// How long one interview turn's model call may take before the fallback question is asked.
const TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

/// Serialises each read-modify-write of an interview: a turn's writer and a second tap overlap.
static INTERVIEW_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

pub(crate) fn repo(state: &AppState) -> Arc<dyn InterestRepository> {
    Arc::new(pond_infra::sqlite_interests::SqliteInterestRepository::new(
        state.db.system.clone(),
    ))
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/interests", get(list).post(create))
        .route("/me/interests/{id}", patch(change).delete(remove))
        .route("/me/interests/interview", get(read_interview).post(start))
        .route("/me/interests/interview/answer", post(answer))
        .route("/me/interests/interview/finish", post(finish))
        .route("/me/interests/interview/approve", post(approve))
        .route("/me/portrait", get(portrait))
}

fn bad_body(rejection: JsonRejection) -> Failure {
    failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text())
}

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let interests = repo(&state).list(&member).await.map_err(server_error)?;
    Ok(Json(json!({ "interests": interests })))
}

async fn create(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<InterestInput>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(input) = body.map_err(bad_body)?;
    if pond_core::user_data::domain::interest::clean_topic(&input.topic).is_none() {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "an interest needs a topic",
        ));
    }
    let row = repo(&state)
        .confirm(&member, &input, Origin::You)
        .await
        .map_err(server_error)?;
    Ok(Json(json!(row)))
}

async fn change(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<InterestPatch>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(patch) = body.map_err(bad_body)?;
    match repo(&state).patch(&member, &id, &patch).await {
        Ok(Some(row)) => Ok(Json(json!(row))),
        Ok(None) => Err(failure(StatusCode::NOT_FOUND, "no such interest")),
        // The unique topic per member: a rename onto another row's topic.
        Err(e) if format!("{e:#}").contains("UNIQUE") => Err(failure(
            StatusCode::CONFLICT,
            "another of your interests already has that topic",
        )),
        Err(e) => Err(server_error(e)),
    }
}

async fn remove(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match repo(&state).delete(&member, &id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(failure(StatusCode::NOT_FOUND, "no such interest")),
        Err(e) => Err(server_error(e)),
    }
}

async fn portrait(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    match repo(&state).portrait(&member).await {
        Ok(Some(portrait)) => Ok(Json(json!(portrait))),
        Ok(None) => Err(failure(
            StatusCode::NOT_FOUND,
            "no portrait yet: the Pond writes one when it is quiet",
        )),
        Err(e) => Err(server_error(e)),
    }
}

// ── The interview ──────────────────────────────────────────────────────

/// The member's interview, recovering one whose writer never came back.
async fn current(state: &Arc<AppState>, member: &str) -> Result<InterestInterview, Failure> {
    let repo = repo(state);
    let interview = repo
        .interview(member)
        .await
        .map_err(server_error)?
        .unwrap_or_default();
    if !interview.thinking_is_stale(Utc::now()) {
        return Ok(interview);
    }
    tracing::warn!(
        target: "giap::trace",
        kind = "interests_interview_recovered",
        "an interview turn outlived its writer; asking the fallback question"
    );
    let recovered = interview::apply_reply(interview, None, Utc::now());
    repo.save_interview(member, &recovered)
        .await
        .map_err(server_error)?;
    Ok(recovered)
}

async fn read_interview(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let _held = INTERVIEW_LOCK.lock().await;
    Ok(Json(json!(current(&state, &member).await?)))
}

async fn start(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let _held = INTERVIEW_LOCK.lock().await;
    let existing = current(&state, &member).await?;
    let (interview, ask) = interview::begin(Some(existing));
    if ask {
        repo(&state)
            .save_interview(&member, &interview)
            .await
            .map_err(server_error)?;
        spawn_turn(state.clone(), member, interview.turns.len());
    }
    Ok(Json(json!(interview)))
}

#[derive(Debug, Default, Deserialize)]
struct AnswerBody {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    choices: Vec<String>,
}

async fn answer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<AnswerBody>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let _held = INTERVIEW_LOCK.lock().await;
    let existing = current(&state, &member).await?;
    let said = interview::answer_text(body.text.as_deref(), &body.choices);
    let thinking = interview::record_answer(existing, said, Utc::now()).map_err(|refusal| {
        let status = match refusal {
            interview::InterviewRefusal::EmptyAnswer => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::CONFLICT,
        };
        failure(status, refusal.as_str())
    })?;
    repo(&state)
        .save_interview(&member, &thinking)
        .await
        .map_err(server_error)?;
    spawn_turn(state.clone(), member, thinking.turns.len());
    Ok(Json(json!(thinking)))
}

async fn finish(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let _held = INTERVIEW_LOCK.lock().await;
    let existing = current(&state, &member).await?;
    if existing.status == InterviewStatus::NotStarted {
        return Err(failure(StatusCode::CONFLICT, "the interview has not started"));
    }
    let reviewed = interview::finish(existing);
    repo(&state)
        .save_interview(&member, &reviewed)
        .await
        .map_err(server_error)?;
    Ok(Json(json!(reviewed)))
}

#[derive(Debug, Default, Deserialize)]
struct ApproveBody {
    #[serde(default)]
    keep: Vec<String>,
    #[serde(default)]
    edits: std::collections::HashMap<String, InterestPatch>,
}

async fn approve(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<ApproveBody>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let _held = INTERVIEW_LOCK.lock().await;
    let mut existing = current(&state, &member).await?;
    if existing.status != InterviewStatus::Review {
        return Err(failure(
            StatusCode::CONFLICT,
            interview::InterviewRefusal::NotInReview.as_str(),
        ));
    }
    let repo = repo(&state);
    let mut saved = 0usize;
    for row in existing.approved(&body.keep, &body.edits) {
        let input = InterestInput {
            topic: row.topic.clone(),
            category: Some(row.category.clone()),
            facets: Some(row.facets.clone()),
            depth: Some(row.depth),
            news: Some(row.news),
            avoid: Some(row.avoid),
        };
        repo.confirm(&member, &input, Origin::Interview)
            .await
            .map_err(server_error)?;
        saved += 1;
    }
    existing.status = InterviewStatus::Done;
    repo.save_interview(&member, &existing)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({ "saved": saved })))
}

/// What the Pond knows of the member, to seed the interview: their portrait, the interests the
/// phone would show (confirmed, offered guesses, avoided), and their weightiest memories.
async fn known_about(state: &AppState, member: &str) -> KnownAboutMember {
    let repo = repo(state);
    let portrait = repo.portrait(member).await.ok().flatten();
    let interests = repo
        .list(member)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.confirmed || i.avoid || i.evidence >= OFFERED_AT)
        .take(20)
        .collect();
    let memories = state
        .memory_repo
        .search_recent(&ProfileScope::Owner(member.to_string()), 80)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.profile_id.as_deref() == Some(member))
        .filter(|m| m.segment != Some(MemorySegment::Knowledge))
        .filter(|m| {
            !pond_core::user_data::services::memory_extraction::describes_a_question(&m.content)
        })
        .collect();
    let memories = pond_core::user_data::services::member_portrait::top_memories(
        memories,
        interview::SEED_MEMORIES,
    )
    .into_iter()
    .map(|m| m.content)
    .collect();
    KnownAboutMember {
        portrait,
        interests,
        memories,
    }
}

pub(crate) async fn member_name(state: &AppState, member: &str) -> String {
    state
        .profile_repo
        .get(member)
        .await
        .ok()
        .flatten()
        .map(|p| p.display_name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "the member".to_string())
}

/// Writes the next question in the background. `turns` is how many turns the interview had when
/// the turn was asked for; a reply to an interview that has moved on since is dropped.
fn spawn_turn(state: Arc<AppState>, member: String, turns: usize) {
    tokio::spawn(async move {
        let repo = repo(&state);
        let Ok(Some(asked)) = repo.interview(&member).await else {
            return;
        };
        let name = member_name(&state, &member).await;
        let known = known_about(&state, &member).await;
        let system = interview::render_system(&name, asked.planned);
        let user = interview::build_user_message(&name, &known, &asked);
        let provider = state.llm_provider.read().await.clone();
        let started = std::time::Instant::now();
        let reply = match provider {
            Some(provider) => match tokio::time::timeout(
                TURN_TIMEOUT,
                provider.complete(&system, vec![ChatMessage::user(user)]),
            )
            .await
            {
                Ok(Ok(reply)) => {
                    let parsed = interview::parse_reply(&reply.content);
                    if parsed.is_none() {
                        tracing::warn!(
                            target: "giap::trace",
                            kind = "interests_interview_unparseable",
                            "the interview's reply held no JSON; asking the fallback question"
                        );
                    }
                    parsed
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "interests interview: the model call failed");
                    None
                }
                Err(_) => {
                    tracing::warn!("interests interview: the model took too long");
                    None
                }
            },
            None => None,
        };
        let _held = INTERVIEW_LOCK.lock().await;
        let Ok(Some(now)) = repo.interview(&member).await else {
            return;
        };
        if now.status != InterviewStatus::Thinking || now.turns.len() != turns {
            return;
        }
        let next = interview::apply_reply(now, reply, Utc::now());
        tracing::info!(
            target: "giap::trace",
            kind = "interests_interview_turn",
            asked = next.asked,
            draft = next.draft.len(),
            status = next.status.as_str(),
            ms = started.elapsed().as_millis() as u64,
            "an interests interview turn was written"
        );
        if let Err(e) = repo.save_interview(&member, &next).await {
            tracing::warn!(error = %e, "interests interview: the turn could not be saved");
        }
    });
}
