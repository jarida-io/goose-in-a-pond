//! The questions Goose asks a member when details are missing: read them by batch, answer one
//! or pass on it. An answer is kept, and feeds what the question was asked for.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::anchor::{Anchor, AnchorState};
use pond_core::user_data::domain::dont_list::{DontRule, RuleOrigin, Strength};
use pond_core::user_data::domain::interest::InterestPatch;
use pond_core::user_data::domain::question::{
    batches, Answer, AnswerRefused, Question, QuestionPurpose, QuestionState, INTEREST_DROP,
    INTEREST_FOLLOW,
};

use crate::anchors::{bad_body, not_yours};
use crate::engine_log::ask_for_refresh;
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

const INTEREST_BATCH: &str = "Things you keep coming back to";
const PROFILE_BATCH: &str = "A few things about you";
const LEDGER_BATCH: &str = "Your spending";

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/questions", get(list))
        .route("/me/questions/{id}/answer", post(answer))
        .route("/me/questions/{id}/skip", post(skip))
}

fn no_such_question() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such question")
}

fn no_longer_open() -> Failure {
    failure(StatusCode::CONFLICT, AnswerRefused::NotOpen.to_string())
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

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let anchors = live_anchors(&state, &member).await?;
    let mut open = state
        .engine
        .questions
        .open(&member)
        .await
        .map_err(server_error)?;
    for anchor in anchors.iter().filter(|anchor| anchor.profile_id != member) {
        open.extend(asked_for_shared(&state, anchor).await?);
    }
    let titles: HashMap<&str, &str> = anchors
        .iter()
        .map(|anchor| (anchor.id.as_str(), anchor.title.as_str()))
        .collect();
    let title_of = |question: &Question| match question.purpose {
        QuestionPurpose::Anchor => question
            .anchor_id
            .as_deref()
            .and_then(|id| titles.get(id))
            .copied()
            .unwrap_or_default(),
        QuestionPurpose::Interest => INTEREST_BATCH,
        QuestionPurpose::Profile => PROFILE_BATCH,
        QuestionPurpose::Ledger => LEDGER_BATCH,
    };
    let now = Utc::now();
    // An anchor that is over has nothing left for an answer to feed.
    open.retain(|question| {
        !question.is_overdue(now)
            && (question.purpose != QuestionPurpose::Anchor || !title_of(question).is_empty())
    });
    let shown = batches(&open, |question| title_of(question).to_string());
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

/// Asks for a refresh of everything the anchor's member can already read, so the answer
/// reaches it.
async fn refresh_for_answer(
    state: &AppState,
    question: &Question,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(anchor_id) = question.anchor_id.as_deref() else {
        return Ok(());
    };
    let anchors = &state.engine.anchors;
    let Some(anchor) = anchors
        .find(anchor_id)
        .await?
        .filter(|anchor| anchor.state == AnchorState::Active)
    else {
        return Ok(());
    };
    for deliverable in anchors.deliverables_of(anchor_id).await? {
        if deliverable.state.is_shown() {
            ask_for_refresh(state, &anchor, &deliverable, now).await?;
        }
    }
    Ok(())
}

async fn answer(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Answer>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(given) = body.map_err(bad_body)?;
    let mut question = reachable(&state, &member, &id).await?;
    let now = Utc::now();
    question
        .answer(given, now)
        .map_err(|refused| match refused {
            AnswerRefused::NotOpen => no_longer_open(),
            refused => failure(StatusCode::UNPROCESSABLE_ENTITY, refused.to_string()),
        })?;
    // Before the answer is kept: a second try after a failure here finds the question open.
    if question.purpose == QuestionPurpose::Interest {
        follow_or_drop(&state, &question, now)
            .await
            .map_err(server_error)?;
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
    match question.purpose {
        // After the answer is kept, so the refresh reads it; the tick refreshes it if this fails.
        QuestionPurpose::Anchor => {
            if let Err(e) = refresh_for_answer(&state, &question, now).await {
                tracing::warn!(error = %e, "a refresh after an answer could not be queued");
            }
        }
        QuestionPurpose::Profile => {
            crate::fact_sheet::rebuild_after_interview(&state, &question.profile_id).await;
        }
        QuestionPurpose::Interest | QuestionPurpose::Ledger => {}
    }
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
    if !question.skip(Utc::now()) {
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
    Ok(StatusCode::NO_CONTENT)
}
