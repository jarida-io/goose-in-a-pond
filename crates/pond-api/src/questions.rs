//! The questions Goose asks a member when details are missing (docs/architecture/
//! structured-questions.md): read them by batch, answer a batch or one question, pass on one, or
//! say enough. An answer is kept, and feeds what the question was asked for: a waiting chat turn
//! carries on, a plan's chain asks its next round, an interest is followed or dropped.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::anchor::{Anchor, AnchorState};
use pond_core::user_data::domain::dont_list::{DontRule, RuleOrigin, Strength};
use pond_core::user_data::domain::engine_job::JobClass;
use pond_core::user_data::domain::interest::InterestPatch;
use pond_core::user_data::domain::question::{
    batches, keep_asking_question, Answer, AnswerRefused, Question, QuestionPurpose, QuestionState,
    INTEREST_DROP, INTEREST_FOLLOW, KEEP_ASKING_GAP, START_NOW,
};
use pond_core::user_data::services::engine::gaps::{
    mark_enough, next_round, request_questions, NextRound,
};
use pond_core::user_data::services::question_desk::{BatchAnswered, DeskRefused};

use crate::anchors::{bad_body, not_yours};
use crate::engine_log::ask_for_refresh;
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

const INTEREST_BATCH: &str = "Things you keep coming back to";
const PROFILE_BATCH: &str = "A few things about you";
const LEDGER_BATCH: &str = "Your spending";
const CHAT_BATCH: &str = "A chat with Goose";

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/questions", get(list))
        .route("/me/questions/batches/{id}/answer", post(answer_batch))
        .route("/me/questions/batches/{id}/enough", post(enough))
        .route("/me/questions/{id}/answer", post(answer))
        .route("/me/questions/{id}/skip", post(skip))
}

fn no_such_question() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such question")
}

fn no_such_batch() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such batch of questions")
}

fn no_longer_open() -> Failure {
    failure(StatusCode::CONFLICT, AnswerRefused::NotOpen.to_string())
}

fn refused(why: DeskRefused) -> Failure {
    match why {
        DeskRefused::NoSuchBatch => no_such_batch(),
        DeskRefused::NotOpen | DeskRefused::Answer(_, AnswerRefused::NotOpen) => no_longer_open(),
        DeskRefused::NoSuchQuestion(_) => failure(StatusCode::NOT_FOUND, why.to_string()),
        DeskRefused::Answer(..) => failure(StatusCode::UNPROCESSABLE_ENTITY, why.to_string()),
    }
}

/// The live anchors the caller owns or is shared with.
async fn live_anchors(state: &AppState, member: &str) -> Result<Vec<Anchor>, Failure> {
    state
        .engine
        .anchors
        .list(member, &AnchorState::LIVE)
        .await
        .map_err(server_error)
}

/// The questions asked for an anchor that is shared with the caller, which they may answer.
async fn asked_for_shared(state: &AppState, anchor: &Anchor) -> Result<Vec<Question>, Failure> {
    state
        .engine
        .questions
        .for_anchor(&anchor.profile_id, &anchor.id)
        .await
        .map_err(server_error)
}

/// The caller's own question, or one asked for an anchor shared with them.
async fn reachable(state: &AppState, member: &str, id: &str) -> Result<Question, Failure> {
    let own = state
        .engine
        .questions
        .get(member, id)
        .await
        .map_err(server_error)?;
    if let Some(question) = own {
        return Ok(question);
    }
    for anchor in live_anchors(state, member).await? {
        if anchor.profile_id == member {
            continue;
        }
        let asked = asked_for_shared(state, &anchor).await?;
        if let Some(question) = asked.into_iter().find(|question| question.id == id) {
            return Ok(question);
        }
    }
    Err(no_such_question())
}

/// The caller's own batch, or one asked for an anchor shared with them, as its questions.
async fn reachable_batch(
    state: &AppState,
    member: &str,
    batch_id: &str,
) -> Result<Vec<Question>, Failure> {
    let own = state
        .engine
        .questions
        .in_batch(member, batch_id)
        .await
        .map_err(server_error)?;
    if !own.is_empty() {
        return Ok(own);
    }
    for anchor in live_anchors(state, member).await? {
        if anchor.profile_id == member {
            continue;
        }
        let asked: Vec<Question> = asked_for_shared(state, &anchor)
            .await?
            .into_iter()
            .filter(|question| question.batch_id == batch_id)
            .collect();
        if !asked.is_empty() {
            return Ok(asked);
        }
    }
    Err(no_such_batch())
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    session_id: Option<String>,
}

/// `GET /me/questions?session_id=`: open and waiting batches, newest first.
async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let only = query
        .session_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let anchors = live_anchors(&state, &member).await?;
    let mut open = state
        .engine
        .questions
        .open(&member)
        .await
        .map_err(server_error)?;
    for anchor in anchors.iter().filter(|anchor| anchor.profile_id != member) {
        open.extend(
            asked_for_shared(&state, anchor)
                .await?
                .into_iter()
                .filter(|q| q.state.is_open()),
        );
    }
    if let Some(session) = &only {
        open.retain(|q| q.session_id.as_deref() == Some(session.as_str()));
    }
    let titles: HashMap<&str, &str> = anchors
        .iter()
        .map(|anchor| (anchor.id.as_str(), anchor.title.as_str()))
        .collect();
    let mut chats: HashMap<String, String> = HashMap::new();
    for session in open.iter().filter_map(|q| q.session_id.as_deref()) {
        if !chats.contains_key(session) {
            let title = match state.session_storage.get_session(session).await {
                Ok(found) => found.title.filter(|t| !t.trim().is_empty()),
                Err(_) => None,
            };
            chats.insert(
                session.to_string(),
                title.unwrap_or_else(|| CHAT_BATCH.into()),
            );
        }
    }
    let title_of = |question: &Question| -> String {
        match question.purpose {
            QuestionPurpose::Anchor => question
                .anchor_id
                .as_deref()
                .and_then(|id| titles.get(id))
                .copied()
                .unwrap_or_default()
                .to_string(),
            QuestionPurpose::Interest => INTEREST_BATCH.into(),
            QuestionPurpose::Profile => PROFILE_BATCH.into(),
            QuestionPurpose::Ledger => LEDGER_BATCH.into(),
            QuestionPurpose::Chat => question
                .session_id
                .as_deref()
                .and_then(|s| chats.get(s))
                .cloned()
                .unwrap_or_else(|| CHAT_BATCH.into()),
        }
    };
    let now = Utc::now();
    // An anchor that is over has nothing left for an answer to feed.
    open.retain(|question| {
        !question.is_overdue(now)
            && (question.purpose != QuestionPurpose::Anchor || !title_of(question).is_empty())
    });
    let shown = batches(&open, title_of);
    Ok(Json(json!({ "batches": shown })))
}

/// What a yes or a no to a guessed interest does to it.
async fn follow_or_drop(
    state: &AppState,
    question: &Question,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let (Some(interest_id), Some(answer)) =
        (question.subject_ref.as_deref(), question.answer.as_ref())
    else {
        return Ok(());
    };
    let member = question.profile_id.as_str();
    let interests = crate::interests::repo(state);
    if answer.chose(INTEREST_FOLLOW) {
        let follow = InterestPatch {
            confirmed: Some(true),
            avoid: Some(false),
            ..InterestPatch::default()
        };
        interests.patch(member, interest_id, &follow).await?;
    } else if answer.chose(INTEREST_DROP) {
        let drop = InterestPatch {
            avoid: Some(true),
            ..InterestPatch::default()
        };
        let Some(interest) = interests.patch(member, interest_id, &drop).await? else {
            return Ok(());
        };
        let rule = DontRule::new(
            member,
            &interest.topic,
            Strength::Avoid,
            RuleOrigin::You,
            now,
        );
        if let Some(mut rule) = rule {
            rule.origin_ref = Some(interest.id);
            state.engine.dont.add(&rule).await?;
        }
    }
    Ok(())
}

/// The plan a question was asked for, while it is active.
async fn active_anchor(state: &AppState, question: &Question) -> anyhow::Result<Option<Anchor>> {
    let Some(anchor_id) = question.anchor_id.as_deref() else {
        return Ok(None);
    };
    Ok(state
        .engine
        .anchors
        .find(anchor_id)
        .await?
        .filter(|anchor| anchor.state == AnchorState::Active))
}

/// Asks for a refresh of everything the anchor's member can already read, so the answer
/// reaches it.
async fn refresh_for_answer(
    state: &AppState,
    anchor: &Anchor,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    for deliverable in state.engine.anchors.deliverables_of(&anchor.id).await? {
        if deliverable.state.is_shown() {
            ask_for_refresh(state, anchor, &deliverable, now).await?;
        }
    }
    Ok(())
}

/// A plan's chain once a batch of it is settled: the soft-limit question, or the next round's
/// job, or nothing.
async fn next_for_plan(
    state: &AppState,
    anchor: &Anchor,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let asked = state
        .engine
        .questions
        .for_anchor(&anchor.profile_id, &anchor.id)
        .await?;
    match next_round(anchor, &asked) {
        NextRound::Stop => {}
        NextRound::Batch { .. } => {
            request_questions(state.engine.jobs.as_ref(), anchor, JobClass::Asked, now).await?;
        }
        NextRound::KeepAsking { round } => {
            let batch_id = uuid::Uuid::new_v4().to_string();
            if let Some(mut keep) =
                keep_asking_question(&anchor.profile_id, &batch_id, &anchor.title, now)
            {
                keep.anchor_id = Some(anchor.id.clone());
                keep.round = round;
                state.engine.questions.add_batch(&[keep]).await?;
                state
                    .engine
                    .questions
                    .mark_asked(&anchor.profile_id, &batch_id, now)
                    .await?;
            }
        }
    }
    Ok(())
}

/// Marks a plan as having enough: its remaining gaps become assumptions.
async fn plan_has_enough(
    state: &AppState,
    anchor: &mut Anchor,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    mark_enough(anchor, now);
    state.engine.anchors.save(anchor).await?;
    Ok(())
}

/// What a kept answer feeds, after it is stored. Failures here are logged, not returned: the
/// answer is kept, and the tick or a later answer picks up what was missed.
async fn after_answers(state: Arc<AppState>, answered: BatchAnswered, now: DateTime<Utc>) {
    let Some(first) = answered.questions.first() else {
        return;
    };
    let fed = async {
        match first.purpose {
            QuestionPurpose::Anchor => {
                if let Some(mut anchor) = active_anchor(&state, first).await? {
                    let start_now = answered.questions.iter().any(|q| {
                        q.gap.as_deref() == Some(KEEP_ASKING_GAP)
                            && q.answer.as_ref().is_some_and(|a| a.chose(START_NOW))
                    });
                    if start_now {
                        plan_has_enough(&state, &mut anchor, now).await?;
                    }
                    refresh_for_answer(&state, &anchor, now).await?;
                    if answered.is_settled() {
                        next_for_plan(&state, &anchor, now).await?;
                    }
                }
            }
            QuestionPurpose::Profile => {
                crate::fact_sheet::rebuild_after_interview(&state, &first.profile_id).await;
            }
            QuestionPurpose::Chat | QuestionPurpose::Interest | QuestionPurpose::Ledger => {}
        }
        anyhow::Ok(())
    };
    if let Err(e) = fed.await {
        tracing::warn!(error = %e, "what an answer feeds could not be queued");
    }
    if let Some((session_id, message)) = answered.late {
        crate::routes::answer_in_session(state, session_id, message).await;
    }
}

/// One answer as the batch route takes it: options by label or by place, and the member's own
/// words.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Choice {
    Place(usize),
    Label(String),
}

#[derive(Debug, Deserialize)]
struct GivenAnswer {
    question_id: String,
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BatchBody {
    answers: Vec<GivenAnswer>,
}

/// The answer as the question's own labels.
fn as_answer(question: Option<&Question>, given: GivenAnswer) -> (String, Answer) {
    let choices = given
        .choices
        .into_iter()
        .map(|choice| match choice {
            Choice::Label(label) => label,
            Choice::Place(at) => question
                .and_then(|q| q.options.get(at))
                .map(|o| o.label.clone())
                .unwrap_or_else(|| at.to_string()),
        })
        .collect();
    (
        given.question_id,
        Answer {
            choices,
            text: given.text,
        },
    )
}

/// An interest answer acts before it is kept: a second try after a failure finds it open.
async fn act_on_interests(
    state: &AppState,
    questions: &[Question],
    answers: &[(String, Answer)],
    now: DateTime<Utc>,
) -> Result<(), Failure> {
    for (id, answer) in answers {
        let Some(question) = questions
            .iter()
            .find(|q| &q.id == id && q.purpose == QuestionPurpose::Interest)
        else {
            continue;
        };
        let mut probe = question.clone();
        if probe.answer(answer.clone(), now).is_ok() {
            follow_or_drop(state, &probe, now)
                .await
                .map_err(server_error)?;
        }
    }
    Ok(())
}

/// `POST /me/questions/batches/{id}/answer`.
async fn answer_batch(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(batch_id): Path<String>,
    body: Result<Json<BatchBody>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    if body.answers.is_empty() {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            AnswerRefused::Empty.to_string(),
        ));
    }
    let questions = reachable_batch(&state, &member, &batch_id).await?;
    let owner = questions[0].profile_id.clone();
    let answers: Vec<(String, Answer)> = body
        .answers
        .into_iter()
        .map(|given| {
            let question = questions.iter().find(|q| q.id == given.question_id);
            as_answer(question, given)
        })
        .collect();
    let now = Utc::now();
    act_on_interests(&state, &questions, &answers, now).await?;
    let answered = state
        .engine
        .desk
        .answer_batch(&owner, &batch_id, answers, now)
        .await
        .map_err(server_error)?
        .map_err(refused)?;
    after_answers(state, answered, now).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/questions/batches/{id}/enough`: go ahead with what Goose has.
async fn enough(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(batch_id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let questions = reachable_batch(&state, &member, &batch_id).await?;
    let first = questions[0].clone();
    // Like a pass: a member a plan is shared with may answer its questions, not end them.
    if first.profile_id != member {
        return Err(not_yours());
    }
    let now = Utc::now();
    state
        .engine
        .desk
        .enough(&first.profile_id, &batch_id, now)
        .await
        .map_err(server_error)?
        .map_err(refused)?;
    if first.purpose == QuestionPurpose::Anchor {
        if let Some(mut anchor) = active_anchor(&state, &first).await.map_err(server_error)? {
            plan_has_enough(&state, &mut anchor, now)
                .await
                .map_err(server_error)?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/questions/{id}/answer`: one question of a batch.
async fn answer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Answer>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(given) = body.map_err(bad_body)?;
    let question = reachable(&state, &member, &id).await?;
    let now = Utc::now();
    let answers = vec![(question.id.clone(), given)];
    act_on_interests(&state, std::slice::from_ref(&question), &answers, now).await?;
    let answered = state
        .engine
        .desk
        .answer_batch(&question.profile_id, &question.batch_id, answers, now)
        .await
        .map_err(server_error)?
        .map_err(refused)?;
    after_answers(state, answered, now).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn skip(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut question = reachable(&state, &member, &id).await?;
    if question.profile_id != member {
        return Err(not_yours());
    }
    if question.state == QuestionState::Skipped {
        return Ok(StatusCode::NO_CONTENT);
    }
    let now = Utc::now();
    if !question.skip(now) {
        return Err(no_longer_open());
    }
    if !state
        .engine
        .questions
        .save(&question)
        .await
        .map_err(server_error)?
    {
        return Err(no_such_question());
    }
    let settled = state
        .engine
        .desk
        .settled(&member, &question.batch_id)
        .await
        .map_err(server_error)?;
    after_answers(state, settled, now).await;
    Ok(StatusCode::NO_CONTENT)
}
