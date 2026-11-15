//! `/me/interview`: the one interview a member gives the Pond.
//!
//! Code asks every question, so an answer returns the next one at once. What the member typed
//! is written up by one direct model call after the last answer; the phone polls `GET` until
//! the draft is there. An interview of taps alone never calls a model.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::models::domain::message::ChatMessage;
use pond_core::security::ports::policy::Principal;
use pond_core::shared::ports::news::NewsStore;
use pond_core::user_data::domain::interest::{InterestInput, Origin};
use pond_core::user_data::domain::interest_interview::InterviewStatus;
use pond_core::user_data::domain::member_profile::About;
use pond_core::user_data::domain::memory::normalise_fact_content;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_core::user_data::services::member_interview::{
    self as interview, AnswerIn, MemberInterview, Refusal, Written, SECTIONS,
};

use crate::about_me::{keep_fact, PROFILE_ROWS};
use crate::interests::repo;
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// How long the write-up's model call may take before the taps alone are written up.
const WRITE_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

/// Serialises each read-modify-write of an interview: the write-up and a second tap overlap.
static INTERVIEW_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/interview", get(read).post(start))
        .route("/me/interview/answer", post(answer))
        .route("/me/interview/finish", post(finish))
        .route("/me/interview/approve", post(approve))
}

fn bad_body(rejection: JsonRejection) -> Failure {
    failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text())
}

fn refused(refusal: Refusal) -> Failure {
    let status = match refusal {
        Refusal::EmptyAnswer => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::CONFLICT,
    };
    failure(status, refusal.as_str())
}

/// The interview as the phone draws it: where it stands, and the sections for a progress bar.
fn view(interview: &MemberInterview) -> Value {
    let (done, total) = interview.progress();
    let mut body = json!(interview);
    body["progress"] = json!({ "done": done, "total": total });
    body["heading"] = json!(SECTIONS.get(interview.section).map(|s| s.heading));
    body["sections"] = json!(SECTIONS
        .iter()
        .map(|s| json!({ "id": s.id, "heading": s.heading }))
        .collect::<Vec<_>>());
    body
}

/// What the member is called in a fact, and every name a fact about them may use.
async fn names(state: &AppState, member: &str) -> (String, Vec<String>) {
    let Ok(Some(profile)) = state.profile_repo.get(member).await else {
        return ("The member".to_string(), Vec::new());
    };
    let mut names = vec![profile.display_name.trim().to_string()];
    if let Some(preferred) = profile.preferences.get("preferred_name") {
        names.push(preferred.trim().to_string());
    }
    names.retain(|n| !n.is_empty());
    match names.first().cloned() {
        Some(name) => (name, names),
        None => ("The member".to_string(), Vec::new()),
    }
}

/// The facts the Pond shows this member today, so a draft repeats none of them.
async fn known_facts(state: &AppState, member: &str) -> Vec<(About, String)> {
    let now = Utc::now();
    state
        .memory_repo
        .search_recent(&ProfileScope::Owner(member.to_string()), PROFILE_ROWS)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.is_shown(now))
        .filter_map(|row| Some((row.about?, row.content)))
        .collect()
}

async fn reviewed(
    state: &AppState,
    member: &str,
    asked: MemberInterview,
    written: Option<Written>,
) -> MemberInterview {
    let (name, aliases) = names(state, member).await;
    let known = known_facts(state, member).await;
    interview::into_review(asked, &name, &aliases, &known, written, Utc::now())
}

async fn save(state: &AppState, member: &str, interview: &MemberInterview) -> Result<(), Failure> {
    repo(state)
        .save_member_interview(member, interview)
        .await
        .map_err(server_error)
}

/// The member's interview, recovering one whose write-up never came back.
async fn current(state: &AppState, member: &str) -> Result<MemberInterview, Failure> {
    let existing = repo(state)
        .member_interview(member)
        .await
        .map_err(server_error)?
        .unwrap_or_default();
    if !existing.thinking_is_stale(Utc::now()) {
        return Ok(existing);
    }
    tracing::warn!(
        target: "giap::trace",
        kind = "member_interview_recovered",
        "an interview's write-up outlived its writer; the taps alone are written up"
    );
    let recovered = reviewed(state, member, existing, None).await;
    save(state, member, &recovered).await?;
    Ok(recovered)
}

/// Save an interview that may have just stopped asking. With nothing typed, or no model to
/// read it, the draft is written here and now; otherwise the write-up runs in the background.
async fn settle(
    state: &Arc<AppState>,
    member: &str,
    asked: MemberInterview,
) -> Result<MemberInterview, Failure> {
    if asked.status != InterviewStatus::Thinking {
        save(state, member, &asked).await?;
        return Ok(asked);
    }
    let has_model = state.llm_provider.read().await.is_some();
    if !asked.has_free_text() || !has_model {
        let draft = reviewed(state, member, asked, None).await;
        save(state, member, &draft).await?;
        return Ok(draft);
    }
    save(state, member, &asked).await?;
    spawn_write_up(state.clone(), member.to_string());
    Ok(asked)
}

async fn read(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let _held = INTERVIEW_LOCK.lock().await;
    Ok(Json(view(&current(&state, &member).await?)))
}

/// `POST /me/interview`: start it, pick it up where it stopped, or begin again after a
/// finished one.
async fn start(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let _held = INTERVIEW_LOCK.lock().await;
    let existing = current(&state, &member).await?;
    let (begun, fresh) = interview::begin(Some(existing), Utc::now());
    if fresh {
        save(&state, &member, &begun).await?;
    }
    Ok(Json(view(&begun)))
}

async fn answer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<AnswerIn>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(said) = body.map_err(bad_body)?;
    let _held = INTERVIEW_LOCK.lock().await;
    let existing = current(&state, &member).await?;
    let next = interview::answer(existing, said, Utc::now()).map_err(refused)?;
    Ok(Json(view(&settle(&state, &member, next).await?)))
}

/// `POST /me/interview/finish`: stop asking and write up what there is.
async fn finish(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let _held = INTERVIEW_LOCK.lock().await;
    let existing = current(&state, &member).await?;
    let stopped = interview::finish(existing).map_err(refused)?;
    Ok(Json(view(&settle(&state, &member, stopped).await?)))
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct ApproveBody {
    /// Ids of the drafted facts to keep.
    #[serde(default)]
    facts: Vec<String>,
    /// Ids of the drafted interests to keep.
    #[serde(default)]
    interests: Vec<String>,
    /// A kept fact's sentence as the member reworded it, by fact id.
    #[serde(default)]
    edits: HashMap<String, String>,
    /// Whether the news answer changes the member's news settings.
    #[serde(default = "yes")]
    news: bool,
}

/// `POST /me/interview/approve`: keep what the member ticked. Facts go into their profile,
/// interests into what the Pond follows for them, and the news answer into their news settings.
async fn approve(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<ApproveBody>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let _held = INTERVIEW_LOCK.lock().await;
    let mut existing = current(&state, &member).await?;
    let (facts, interests) =
        interview::approved(&existing, &body.facts, &body.interests).map_err(refused)?;

    let mut kept_facts = 0usize;
    for fact in facts {
        // A sentence the member reworded is theirs to word, as on `/me/about`.
        let content = match body.edits.get(&fact.id) {
            Some(reworded) => normalise_fact_content(reworded),
            None => fact.content.clone(),
        };
        if content.is_empty() {
            continue;
        }
        let (_, created) =
            keep_fact(&state, &member, fact.about, &content, false, "interview").await?;
        kept_facts += usize::from(created);
    }

    let interest_repo = repo(&state);
    let mut kept_interests = 0usize;
    for row in interests {
        let input = InterestInput {
            topic: row.topic.clone(),
            category: Some(row.category.clone()),
            facets: Some(row.facets.clone()),
            depth: Some(row.depth),
            news: Some(row.news),
            avoid: Some(row.avoid),
        };
        interest_repo
            .confirm(&member, &input, Origin::Interview)
            .await
            .map_err(server_error)?;
        kept_interests += 1;
    }

    let mut news_set = false;
    if let Some(wish) = existing.news.filter(|_| body.news) {
        let news = crate::news::store(&state);
        let mut settings = news.settings(&member).await.map_err(server_error)?;
        (settings.tiles, settings.digest) = wish.tiles_and_digest();
        news.save_settings(&member, &settings)
            .await
            .map_err(server_error)?;
        news_set = true;
    }

    existing.status = InterviewStatus::Done;
    save(&state, &member, &existing).await?;
    tracing::info!(
        target: "giap::trace",
        kind = "member_interview_approved",
        facts = kept_facts,
        interests = kept_interests,
        news = news_set,
        "an interview's draft was kept"
    );
    // What they kept changes what ideas should build on; a busy lane runs it later anyway.
    if let Some(lane) = state.lane.as_ref().filter(|_| kept_interests > 0) {
        let _ = lane.wake(LaneJob::Ideas).await;
    }
    Ok(Json(json!({
        "facts": kept_facts,
        "interests": kept_interests,
        "news": news_set,
    })))
}

/// Writes the draft in the background from what the member typed. A reply to an interview
/// that has moved on since is dropped.
fn spawn_write_up(state: Arc<AppState>, member: String) {
    tokio::spawn(async move {
        let interviews = repo(&state);
        let Ok(Some(asked)) = interviews.member_interview(&member).await else {
            return;
        };
        let (name, _) = names(&state, &member).await;
        let provider = state.llm_provider.read().await.clone();
        let started = std::time::Instant::now();
        let written = match (
            provider,
            interview::build_user_message(&name, &asked.answers),
        ) {
            (Some(provider), Some(user)) => {
                let system = interview::render_system(&name);
                match tokio::time::timeout(
                    WRITE_UP_TIMEOUT,
                    provider.complete(&system, vec![ChatMessage::user(user)]),
                )
                .await
                {
                    Ok(Ok(reply)) => {
                        let parsed = interview::parse_reply(&reply.content);
                        if parsed.is_none() {
                            tracing::warn!(
                                target: "giap::trace",
                                kind = "member_interview_unparseable",
                                "the write-up held no JSON; the taps alone are written up"
                            );
                        }
                        parsed
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "member interview: the model call failed");
                        None
                    }
                    Err(_) => {
                        tracing::warn!("member interview: the model took too long");
                        None
                    }
                }
            }
            _ => None,
        };
        let _held = INTERVIEW_LOCK.lock().await;
        let Ok(Some(now)) = interviews.member_interview(&member).await else {
            return;
        };
        if now.status != InterviewStatus::Thinking || now.answers != asked.answers {
            return;
        }
        let draft = reviewed(&state, &member, now, written).await;
        tracing::info!(
            target: "giap::trace",
            kind = "member_interview_written",
            facts = draft.facts.len(),
            interests = draft.interests.len(),
            ms = started.elapsed().as_millis() as u64,
            "an interview's draft was written"
        );
        if let Err(e) = interviews.save_member_interview(&member, &draft).await {
            tracing::warn!(error = %e, "member interview: the draft could not be saved");
        }
    });
}
