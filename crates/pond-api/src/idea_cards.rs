//! A member's idea cards (docs/architecture/idea-cards.md): the set on show, a refresh, and what
//! each button does. The chat's context cards are `context_cards`, under `/me/cards`.

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::context_card::ContextCard;
use pond_core::shared::domain::todo::{zone, TodoSource};
use pond_core::user_data::domain::anchor::{
    Anchor, AnchorFact, AnchorKind, AnchorOrigin, AnchorState, Deliverable,
};
use pond_core::user_data::domain::card::{
    ActionKind, Card, CardKind, CardNotes, CardSource, CardSourceKind, CardState, Why, WhySource,
};
use pond_core::user_data::domain::dont_list::{Reason, RuleOrigin, Strength};
use pond_core::user_data::domain::engine_job::{JobClass, JobKind, JobState};
use pond_core::user_data::services::anchor_add::{add_asked, AnchorAdded};
use pond_core::user_data::services::cards::{due_time, jobs_ahead, request_cards};
use pond_core::user_data::services::engine::gaps::request_questions;
use pond_core::user_data::services::engine::recipes::{deliverable_title, slots_of};

use crate::anchors::{bad_body, members_words, turned_down};
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// What the Go deeper plan's notes are kept under, and who wrote them.
const NOTES_LABEL: &str = "From the card";
const NOTES_SOURCE: &str = "goose";

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/idea-cards", get(list).post(create))
        .route("/me/idea-cards/refresh", post(refresh))
        .route("/me/idea-cards/{id}/do", post(do_it))
        .route("/me/idea-cards/{id}/deeper", post(deeper))
        .route("/me/idea-cards/{id}/save", post(save))
        .route("/me/idea-cards/{id}/unsave", post(unsave))
        .route("/me/idea-cards/{id}/dismiss", post(dismiss))
}

fn no_such_card() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such card")
}

fn settled() -> Failure {
    failure(
        StatusCode::CONFLICT,
        "this card is done or turned down already",
    )
}

/// The member's card that is still theirs to act on.
async fn open_card(state: &AppState, member: &str, id: &str) -> Result<Card, Failure> {
    let card = state
        .engine
        .cards
        .get(member, id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_card)?;
    if matches!(card.state, CardState::Done | CardState::Dismissed) {
        return Err(settled());
    }
    Ok(card)
}

async fn moved(state: &AppState, card: &mut Card, to: CardState) -> Result<(), Failure> {
    card.set_state(to).map_err(|_| settled())?;
    if !state.engine.cards.save(card).await.map_err(server_error)? {
        return Err(no_such_card());
    }
    Ok(())
}

/// Where the member's next set stands, and when the newest on show was made.
async fn set_view(state: &AppState, member: &str, cards: &[Card]) -> Result<Value, Failure> {
    let open = state
        .engine
        .jobs
        .open_of_kind(member, JobKind::Cards)
        .await
        .map_err(server_error)?;
    let made_at: Option<DateTime<Utc>> = cards.iter().map(|card| card.created_at).max();
    let word = match open.first().map(|job| job.state) {
        Some(JobState::Queued) => "queued",
        Some(_) => "making",
        None if made_at.is_some() => "ready",
        None => "none",
    };
    let mut view = json!({"state": word, "made_at": made_at});
    // A queued set says what it waits behind, so a refresh behind a long plan does not read as stuck.
    if let Some(waiting) = open.first().filter(|job| job.state == JobState::Queued) {
        let all = state.engine.jobs.open().await.map_err(server_error)?;
        view["ahead"] = json!(jobs_ahead(&all, waiting));
    }
    Ok(view)
}

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let cards = state
        .engine
        .cards
        .shown(&member, Utc::now())
        .await
        .map_err(server_error)?;
    let set = set_view(&state, &member, &cards).await?;
    Ok(Json(json!({"cards": cards, "set": set})))
}

#[derive(Debug, Deserialize)]
struct CreateIdeaBody {
    title: String,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

async fn create(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<CreateIdeaBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Card>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(body) = body.map_err(bad_body)?;
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return Err(failure(StatusCode::BAD_REQUEST, "title is required"));
    }
    let why_text = body
        .why
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Your own idea")
        .to_string();
    let notes_text = body
        .notes
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let kind = body
        .kind
        .as_deref()
        .and_then(CardKind::parse)
        .unwrap_or(CardKind::Do);
    let now = Utc::now();
    let card = Card {
        id: uuid::Uuid::new_v4().to_string(),
        profile_id: member,
        batch_id: "user".to_string(),
        rank: 0,
        kind,
        title,
        why: Why {
            text: why_text,
            source: WhySource {
                kind: CardSourceKind::Memory,
                label: "Your idea".to_string(),
                url: None,
            },
        },
        when: None,
        where_: None,
        cost: None,
        effort: None,
        action: None,
        sources: vec![CardSource {
            kind: CardSourceKind::Memory,
            title: "Added by you".to_string(),
            url: None,
        }],
        notes: CardNotes {
            found: notes_text,
            next: String::new(),
            caveats: String::new(),
        },
        anchor: None,
        state: CardState::Saved,
        created_at: now,
        fades_at: Card::fade_time(now),
    };
    state
        .engine
        .cards
        .add_one(&card)
        .await
        .map_err(server_error)?;
    Ok((StatusCode::CREATED, Json(card)))
}

async fn refresh(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let job = request_cards(
        state.engine.jobs.as_ref(),
        &member,
        JobClass::Asked,
        Utc::now(),
    )
    .await
    .map_err(server_error)?;
    let word = if job.state == JobState::Queued {
        "queued"
    } else {
        "making"
    };
    Ok((StatusCode::ACCEPTED, Json(json!({"state": word}))))
}

/// The plan the card belongs to, or one made from the card, with its first deliverable and its
/// questions queued as asked for. Returns the plan's id.
async fn go_deeper(state: &AppState, member: &str, card: &Card) -> Result<String, Failure> {
    let now = Utc::now();
    let held = match &card.anchor {
        Some(named) => state
            .engine
            .anchors
            .get(member, &named.id)
            .await
            .map_err(server_error)?
            .filter(|anchor| anchor.profile_id == member && anchor.state == AnchorState::Active),
        None => None,
    };
    let anchor = match held {
        Some(anchor) => anchor,
        None => made_plan(state, member, card, now).await?,
    };
    let Some(slot) = slots_of(&anchor).first() else {
        return Ok(anchor.id);
    };
    let existing = state
        .engine
        .anchors
        .deliverables_of(&anchor.id)
        .await
        .map_err(server_error)?;
    let deliverable = match existing.into_iter().find(|d| d.slot == slot.slot) {
        Some(found) => found,
        None => {
            let title = deliverable_title(&anchor, slot);
            let made = Deliverable::new(&anchor.id, slot.kind, slot.slot, &title, now)
                .ok_or_else(|| server_error("the plan's first deliverable has no title"))?;
            state
                .engine
                .anchors
                .add_deliverable(&made)
                .await
                .map_err(server_error)?;
            made
        }
    };
    crate::engine_log::ask_for_refresh(state, &anchor, &deliverable, now)
        .await
        .map_err(server_error)?;
    // Research does not wait for the answers: it states its assumptions until they come.
    request_questions(state.engine.jobs.as_ref(), &anchor, JobClass::Asked, now)
        .await
        .map_err(server_error)?;
    Ok(anchor.id)
}

async fn made_plan(
    state: &AppState,
    member: &str,
    card: &Card,
    now: DateTime<Utc>,
) -> Result<Anchor, Failure> {
    let title = card.anchor.as_ref().map_or(&card.title, |a| &a.title);
    let mut anchor = Anchor::new(member, AnchorKind::Project, title, AnchorOrigin::You, now)
        .ok_or_else(|| server_error("the card has no title"))?;
    anchor.place = card.where_.clone();
    anchor.why = card.why.text.clone();
    if !card.notes.found.trim().is_empty() {
        anchor.set_fact(
            AnchorFact::new(NOTES_LABEL, &card.notes.found, NOTES_SOURCE),
            now,
        );
    }
    let added = add_asked(
        state.engine.anchors.as_ref(),
        state.engine.dont.as_ref(),
        anchor,
        now,
    )
    .await
    .map_err(server_error)?;
    match added {
        AnchorAdded::Ended(_) => Err(failure(
            StatusCode::CONFLICT,
            "you had a plan called that and it has ended",
        )),
        added => Ok(added.anchor().clone()),
    }
}

/// `POST /me/idea-cards/{id}/do`: the card's own action, and the card marked done.
async fn do_it(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut card = open_card(&state, &member, &id).await?;
    let Some(action) = card.action.clone() else {
        return Err(failure(
            StatusCode::CONFLICT,
            "this card has nothing to do: go deeper instead",
        ));
    };
    let mut result = json!({"kind": action.kind});
    match action.kind {
        ActionKind::Todo | ActionKind::Reminder => {
            let tz = zone(
                &state
                    .settings_repo
                    .get()
                    .await
                    .map_err(server_error)?
                    .timezone,
            );
            let mut body = json!({
                "kind": if action.kind == ActionKind::Todo { "todo" } else { "reminder" },
                "text": action.text.as_deref().unwrap_or(&card.title),
            });
            if !card.notes.found.trim().is_empty() {
                body["notes"] = json!(card.notes.found);
            }
            if let Some(due) = action.due.as_deref().and_then(|due| due_time(due, tz)) {
                body["due_at"] = json!(due.to_rfc3339());
            }
            if let Some(anchor) = &card.anchor {
                body["anchor_id"] = json!(anchor.id);
            }
            let added = crate::todos::add(
                &state,
                &member,
                &body,
                TodoSource::Idea,
                Some(card.id.clone()),
            )
            .await?;
            result["todo"] = added.todo().view();
        }
        ActionKind::Chat => {
            result["prompt"] = json!(action.prompt.as_deref().unwrap_or(&card.title));
        }
        ActionKind::Watch => {
            let address = action
                .url
                .clone()
                .ok_or_else(|| failure(StatusCode::CONFLICT, "this card names no page to watch"))?;
            let watch_id =
                crate::ideas::watch_page(&state, &member, address, &card.title, &card.why.text)
                    .await?;
            result["watch_id"] = json!(watch_id);
        }
        ActionKind::GoDeeper => {
            result["anchor_id"] = json!(go_deeper(&state, &member, &card).await?);
        }
    }
    moved(&state, &mut card, CardState::Done).await?;
    Ok(Json(json!({"card": card, "result": result})))
}

/// `POST /me/idea-cards/{id}/deeper`: Go deeper on any card, whatever its action.
async fn deeper(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut card = open_card(&state, &member, &id).await?;
    let anchor_id = go_deeper(&state, &member, &card).await?;
    moved(&state, &mut card, CardState::Done).await?;
    Ok(Json(json!({"card": card, "anchor_id": anchor_id})))
}

async fn save(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Card>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut card = open_card(&state, &member, &id).await?;
    moved(&state, &mut card, CardState::Saved).await?;
    Ok(Json(card))
}

async fn unsave(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Card>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut card = open_card(&state, &member, &id).await?;
    moved(&state, &mut card, CardState::New).await?;
    Ok(Json(card))
}

#[derive(Debug, Default, Deserialize)]
struct Dismissal {
    #[serde(default)]
    reasons: Vec<Reason>,
    #[serde(default)]
    text: Option<String>,
}

/// `POST /me/idea-cards/{id}/dismiss`: "Not this", kept as an avoid rule.
async fn dismiss(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: Result<Json<Dismissal>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let body = match body {
        Ok(Json(body)) => body,
        Err(JsonRejection::MissingJsonContentType(_)) => Dismissal::default(),
        Err(rejection) => return Err(bad_body(rejection)),
    };
    let mut card = state
        .engine
        .cards
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_card)?;
    if card.state == CardState::Dismissed {
        return Ok(StatusCode::NO_CONTENT);
    }
    let now = Utc::now();
    let rule = turned_down(
        &member,
        &card.title,
        Strength::Avoid,
        &body.reasons,
        members_words(body.text.as_deref()),
        (RuleOrigin::Reject, &card.id),
        now,
    );
    // The rule first: a second try after a failed dismiss merges into it.
    if let Some(rule) = rule {
        state.engine.dont.add(&rule).await.map_err(server_error)?;
    }
    moved(&state, &mut card, CardState::Dismissed).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The card as a chat about it is given it: what the member saw, and Goose's notes.
pub(crate) fn as_context(card: &Card) -> ContextCard {
    let mut content = format!("{}\n", card.why.text);
    for (label, value) in [
        ("When", &card.when),
        ("Where", &card.where_),
        ("Cost", &card.cost),
        ("Effort", &card.effort),
    ] {
        if let Some(value) = value {
            content.push_str(&format!("{label}: {value}\n"));
        }
    }
    let mut notes = String::new();
    for (label, value) in [
        ("Found", &card.notes.found),
        ("Next", &card.notes.next),
        ("Unsure", &card.notes.caveats),
    ] {
        if !value.trim().is_empty() {
            notes.push_str(&format!("{label}: {value}\n"));
        }
    }
    for source in &card.sources {
        match &source.url {
            Some(url) => notes.push_str(&format!("Source: {} ({url})\n", source.title)),
            None => notes.push_str(&format!("Source: {}\n", source.title)),
        }
    }
    ContextCard::new("Idea".to_string(), card.title.clone(), content, notes)
}
