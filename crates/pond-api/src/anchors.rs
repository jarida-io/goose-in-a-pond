//! A member's anchors: the real things in their life Goose makes work for. The member adds,
//! corrects, confirms, stops and shares them here; what is made for one is only asked for, and
//! the engine's runner makes it.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::anchor::{
    clean_title, title_key, Anchor, AnchorFact, AnchorKind, AnchorOrigin, AnchorState, Deliverable,
};
use pond_core::user_data::domain::dont_list::{DontRule, Reason, RuleOrigin, Strength};
use pond_core::user_data::domain::engine_job::EngineJob;
use pond_core::user_data::domain::question::{QuestionState, QuestionView};
use pond_core::user_data::services::anchor_add::{add_asked, AnchorAdded};

use crate::engine_log::{call_off, job_steps, job_summary, shown_job};
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

const NOTES_LABEL: &str = "Notes";
/// The source of a fact the member wrote themselves.
const SAID_BY_MEMBER: &str = "you";
const MAX_SAID_CHARS: usize = 400;
const MAX_NOTES_CHARS: usize = 1000;
const MAX_PLACE_CHARS: usize = 120;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/anchors", get(list_anchors).post(add_anchor))
        .route("/me/anchors/{id}", get(read_anchor).patch(change_anchor))
        .route("/me/anchors/{id}/confirm", post(confirm_anchor))
        .route("/me/anchors/{id}/dismiss", post(dismiss_anchor))
        .route("/me/anchors/{id}/stop", post(stop_anchor))
        .route("/me/anchors/{id}/shares", put(share_anchor))
}

pub(crate) fn bad_body(rejection: JsonRejection) -> Failure {
    failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text())
}

fn no_such_anchor() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such anchor")
}

/// What a member is told when they try to change something that is only shared with them.
pub(crate) fn not_yours() -> Failure {
    failure(
        StatusCode::FORBIDDEN,
        "this is shared with you: only the member who owns it can change it",
    )
}

fn already_over() -> Failure {
    failure(
        StatusCode::CONFLICT,
        "this is over: add it again if you still want it",
    )
}

fn untitled() -> Failure {
    failure(
        StatusCode::UNPROCESSABLE_ENTITY,
        "it needs a title: say what it is in a few words",
    )
}

fn ends_before_it_starts() -> Failure {
    failure(
        StatusCode::UNPROCESSABLE_ENTITY,
        "it cannot end before it starts: check the two dates",
    )
}

fn one_line(text: Option<&str>, max_chars: usize) -> Option<String> {
    let one_line: String = text?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect();
    (!one_line.is_empty()).then_some(one_line)
}

/// The member's own words as a rule keeps them: one line, bounded. `None` when they gave none.
pub(crate) fn members_words(text: Option<&str>) -> Option<String> {
    one_line(text, MAX_SAID_CHARS)
}

/// The rule a stop or a rejection leaves: what was turned down, with the member's chips and
/// their own words.
pub(crate) fn turned_down(
    member: &str,
    what: &str,
    strength: Strength,
    reasons: &[Reason],
    said: Option<String>,
    (origin, origin_ref): (RuleOrigin, &str),
    now: DateTime<Utc>,
) -> Option<DontRule> {
    let mut rule = DontRule::avoid_title(member, what, reasons, origin, origin_ref, now)?;
    rule.strength = strength;
    rule.said = said;
    Some(rule)
}

/// An anchor the caller owns or is shared with.
async fn readable(state: &AppState, member: &str, id: &str) -> Result<Anchor, Failure> {
    state
        .engine
        .anchors
        .get(member, id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_anchor)
}

/// An anchor the caller owns, so may change.
async fn owned(state: &AppState, member: &str, id: &str) -> Result<Anchor, Failure> {
    let anchor = readable(state, member, id).await?;
    if anchor.profile_id != member {
        return Err(not_yours());
    }
    Ok(anchor)
}

async fn save(state: &AppState, anchor: &Anchor) -> Result<(), Failure> {
    if !state
        .engine
        .anchors
        .save(anchor)
        .await
        .map_err(server_error)?
    {
        return Err(no_such_anchor());
    }
    Ok(())
}

pub(crate) fn deliverable_summary(deliverable: &Deliverable, mine: bool) -> Value {
    json!({
        "id": deliverable.id,
        "kind": deliverable.kind,
        "slot": deliverable.slot,
        "title": deliverable.title,
        "state": deliverable.state,
        "hook": deliverable.hook,
        "version": deliverable.version,
        "changed": deliverable.changed,
        // Only its owner can mark it seen, so only they are shown it as new.
        "unseen": mine && deliverable.is_unseen(),
    })
}

/// An anchor with what hangs off it. `whole` adds its open questions and the steps of its job.
async fn view(
    state: &AppState,
    member: &str,
    anchor: &Anchor,
    open_jobs: &[EngineJob],
    whole: bool,
) -> Result<Value, Failure> {
    let mine = anchor.profile_id == member;
    let shared_by = if mine {
        None
    } else {
        state
            .profile_repo
            .get(&anchor.profile_id)
            .await
            .map_err(server_error)?
            .map(|owner| owner.display_name)
    };
    let deliverables = state
        .engine
        .anchors
        .deliverables(member, &anchor.id)
        .await
        .map_err(server_error)?;
    let questions: Vec<QuestionView> = state
        .engine
        .questions
        .for_anchor(&anchor.profile_id, &anchor.id)
        .await
        .map_err(server_error)?
        .into_iter()
        .filter(|question| question.state == QuestionState::Open)
        .map(|question| QuestionView {
            id: question.id,
            text: question.text,
            options: question.options,
            multi: question.multi,
        })
        .collect();
    let job = shown_job(open_jobs, &anchor.id);
    let mut job_view = job.map(job_summary);
    let mut out = json!({
        "id": anchor.id,
        "kind": anchor.kind,
        "title": anchor.title,
        "state": anchor.state,
        "starts_at": anchor.starts_at,
        "ends_at": anchor.ends_at,
        "place": anchor.place,
        "facts": anchor.facts,
        "agent": anchor.agent,
        "origin": anchor.origin,
        "why": anchor.why,
        "confirmed_at": anchor.confirmed_at,
        "ended_at": anchor.ended_at,
        "created_at": anchor.created_at,
        "updated_at": anchor.updated_at,
        "mine": mine,
        // Only ever the caller's own id, so a share list can leave its owner out.
        "owner_id": mine.then(|| anchor.profile_id.clone()),
        "shared_by": shared_by,
        "shares": anchor.shared_with,
        "deliverables": deliverables
            .iter()
            .map(|deliverable| deliverable_summary(deliverable, mine))
            .collect::<Vec<_>>(),
        "open_questions": questions.len(),
    });
    if whole {
        out["questions"] = json!(questions);
        if let (Some(job), Some(job_view)) = (job, job_view.as_mut()) {
            job_view["steps"] = json!(job_steps(state, job).await.map_err(server_error)?);
        }
    }
    out["job"] = json!(job_view);
    Ok(out)
}

/// The anchor as it now stands, whole.
async fn answer_with(state: &AppState, member: &str, id: &str) -> Result<Json<Value>, Failure> {
    let anchor = readable(state, member, id).await?;
    let open = state.engine.jobs.open().await.map_err(server_error)?;
    Ok(Json(view(state, member, &anchor, &open, true).await?))
}

async fn list_anchors(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let anchors = state
        .engine
        .anchors
        .list(&member, &AnchorState::LIVE)
        .await
        .map_err(server_error)?;
    let open = state.engine.jobs.open().await.map_err(server_error)?;
    let mut shown = Vec::with_capacity(anchors.len());
    for anchor in &anchors {
        shown.push(view(&state, &member, anchor, &open, false).await?);
    }
    Ok(Json(json!({ "anchors": shown })))
}

async fn read_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    answer_with(&state, &member, &id).await
}

fn place_of(raw: Option<String>) -> Option<String> {
    one_line(raw.as_deref(), MAX_PLACE_CHARS)
}

#[derive(Debug, Deserialize)]
struct NewAnchor {
    kind: AnchorKind,
    title: String,
    #[serde(default)]
    starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    ends_at: Option<DateTime<Utc>>,
    #[serde(default)]
    place: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

async fn add_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<NewAnchor>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    if body.kind == AnchorKind::Standing {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "choose an event, a trip, a purchase, a project or an interest",
        ));
    }
    let now = Utc::now();
    let mut anchor = Anchor::new(&member, body.kind, &body.title, AnchorOrigin::You, now)
        .ok_or_else(untitled)?;
    if matches!((body.starts_at, body.ends_at), (Some(start), Some(end)) if end < start) {
        return Err(ends_before_it_starts());
    }
    anchor.starts_at = body.starts_at;
    anchor.ends_at = body.ends_at;
    anchor.place = place_of(body.place);
    if let Some(notes) = one_line(body.notes.as_deref(), MAX_NOTES_CHARS) {
        anchor.set_fact(AnchorFact::new(NOTES_LABEL, &notes, SAID_BY_MEMBER), now);
    }

    let added = add_asked(
        state.engine.anchors.as_ref(),
        state.engine.dont.as_ref(),
        anchor,
        now,
    )
    .await
    .map_err(server_error)?;
    let status = match &added {
        AnchorAdded::New(_) | AnchorAdded::Revived(_) => StatusCode::CREATED,
        AnchorAdded::Held(_) | AnchorAdded::Confirmed(_) => StatusCode::OK,
        AnchorAdded::Ended(_) => {
            return Err(failure(
                StatusCode::CONFLICT,
                "you had one called that and it has ended: give this one another name",
            ))
        }
    };
    Ok((
        status,
        answer_with(&state, &member, &added.anchor().id).await?,
    ))
}

/// A field left out of the body stays as it is; one sent as `null` is cleared.
fn sent<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
struct AnchorPatch {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, deserialize_with = "sent")]
    starts_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "sent")]
    ends_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "sent")]
    place: Option<Option<String>>,
    /// The whole list as the member leaves it, so a wrong fact can be taken off.
    #[serde(default)]
    facts: Option<Vec<AnchorFact>>,
}

async fn change_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<AnchorPatch>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(patch) = body.map_err(bad_body)?;
    let mut anchor = owned(&state, &member, &id).await?;
    if !AnchorState::LIVE.contains(&anchor.state) {
        return Err(already_over());
    }
    let now = Utc::now();
    if let Some(title) = patch.title.as_deref() {
        let title = clean_title(title).ok_or_else(untitled)?;
        let taken = state
            .engine
            .anchors
            .by_title(&member, &title_key(&title))
            .await
            .map_err(server_error)?
            .is_some_and(|other| other.id != anchor.id);
        if taken {
            return Err(failure(
                StatusCode::CONFLICT,
                "you have, or had, another one called that: give this one another name",
            ));
        }
        anchor.title = title;
    }
    if let Some(starts_at) = patch.starts_at {
        anchor.starts_at = starts_at;
    }
    if let Some(ends_at) = patch.ends_at {
        anchor.ends_at = ends_at;
    }
    if matches!((anchor.starts_at, anchor.ends_at), (Some(start), Some(end)) if end < start) {
        return Err(ends_before_it_starts());
    }
    if let Some(place) = patch.place {
        anchor.place = place_of(place);
    }
    if let Some(facts) = patch.facts {
        anchor.facts.clear();
        for fact in facts {
            let source = match fact.source.trim() {
                "" => SAID_BY_MEMBER,
                source => source,
            };
            let fact = AnchorFact::new(&fact.label, &fact.value, source);
            if !fact.label.is_empty() && !fact.value.is_empty() {
                anchor.set_fact(fact, now);
            }
        }
    }
    anchor.updated_at = now;
    save(&state, &anchor).await?;
    answer_with(&state, &member, &id).await
}

async fn confirm_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut anchor = owned(&state, &member, &id).await?;
    match anchor.state {
        AnchorState::Active => {}
        AnchorState::Candidate => {
            anchor.confirm(Utc::now()).map_err(server_error)?;
            save(&state, &anchor).await?;
        }
        _ => return Err(already_over()),
    }
    answer_with(&state, &member, &id).await
}

async fn dismiss_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut anchor = owned(&state, &member, &id).await?;
    match anchor.state {
        AnchorState::Faded => {}
        AnchorState::Candidate => {
            anchor.dismiss(Utc::now()).map_err(server_error)?;
            save(&state, &anchor).await?;
        }
        AnchorState::Active => {
            return Err(failure(
                StatusCode::CONFLICT,
                "you already said yes to this: stop it instead",
            ))
        }
        _ => return Err(already_over()),
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize)]
struct Stop {
    #[serde(default)]
    reasons: Vec<Reason>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    never: bool,
}

async fn stop_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Stop>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = match body {
        Ok(Json(body)) => body,
        // A stop with no body is a stop with no reason.
        Err(JsonRejection::MissingJsonContentType(_)) => Stop::default(),
        Err(rejection) => return Err(bad_body(rejection)),
    };
    let mut anchor = owned(&state, &member, &id).await?;
    let now = Utc::now();
    match anchor.state {
        AnchorState::Stopped => {}
        AnchorState::Active => {
            let said = members_words(body.text.as_deref());
            if body.never || !body.reasons.is_empty() || said.is_some() {
                let strength = if body.never {
                    Strength::Never
                } else {
                    Strength::Avoid
                };
                let rule = turned_down(
                    &member,
                    &anchor.title,
                    strength,
                    &body.reasons,
                    said,
                    (RuleOrigin::Stop, &anchor.id),
                    now,
                );
                // The rule first: a second try after a failed stop merges into it.
                if let Some(rule) = rule {
                    state.engine.dont.add(&rule).await.map_err(server_error)?;
                }
            }
            anchor.stop(now).map_err(server_error)?;
            save(&state, &anchor).await?;
        }
        AnchorState::Candidate => {
            return Err(failure(
                StatusCode::CONFLICT,
                "you have not said yes to this yet: dismiss it instead",
            ))
        }
        _ => return Err(already_over()),
    }
    let open = state.engine.jobs.open().await.map_err(server_error)?;
    for job in open
        .into_iter()
        .filter(|job| job.anchor_id.as_deref() == Some(anchor.id.as_str()))
    {
        call_off(&state, job, "Stopped by the member.", now)
            .await
            .map_err(server_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct Shares {
    profile_ids: Vec<String>,
}

async fn share_anchor(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Shares>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let anchor = owned(&state, &member, &id).await?;
    let members: HashSet<String> = state
        .profile_repo
        .list()
        .await
        .map_err(server_error)?
        .into_iter()
        .map(|profile| profile.id)
        .collect();
    if let Some(stranger) = body.profile_ids.iter().find(|id| !members.contains(*id)) {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{stranger}` is not a member of this pond: choose from its members"),
        ));
    }
    if !state
        .engine
        .anchors
        .set_shares(&member, &anchor.id, &body.profile_ids)
        .await
        .map_err(server_error)?
    {
        return Err(no_such_anchor());
    }
    answer_with(&state, &member, &id).await
}
