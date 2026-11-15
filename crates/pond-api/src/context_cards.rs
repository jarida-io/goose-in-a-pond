//! Cards a member sends to chat: an idea, a news story, a digest, a to-do, a watched link. The
//! phone names the card by kind and id; the Pond reads it as that member and gives the turn
//! what they read with the notes Goose kept, so no client writes what the model is told.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::cart::CartItem;
use pond_core::shared::domain::context_card::ContextCard;
use pond_core::shared::domain::todo::{Todo, TodoKind, TodoState};
use pond_core::shared::ports::news::NewsStore;
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::shared::ports::web_watch::{StoredChange, WebWatch, WebWatchRepository};
use pond_infra::sqlite_web_watch::SqliteWebWatchRepository;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// A card as a chat request names it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CardRef {
    /// `idea`, `news`, `digest`, `todo`, `cart` or `watch`.
    pub kind: String,
    pub id: String,
}

pub(crate) const KINDS: [&str; 6] = ["idea", "news", "digest", "todo", "cart", "watch"];

/// Changes a watch's card tells Goose about.
const WATCH_CHANGES: usize = 8;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/me/cards/{kind}/{id}", get(read))
}

fn todo_card(todo: &Todo) -> ContextCard {
    let kind = match todo.kind {
        TodoKind::Reminder => "Reminder",
        _ => "To-do",
    };
    let mut content = String::new();
    if let Some(notes) = todo.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        content.push_str(notes);
    }
    let mut notes = match todo.state {
        TodoState::Open => "It is open.\n".to_string(),
        _ => "It is closed.\n".to_string(),
    };
    if let Some(due) = todo.due_at {
        notes.push_str(&format!("Due {} UTC.\n", due.format("%-d %B %Y %H:%M")));
    }
    if !todo.hooks.is_empty() {
        notes.push_str(&format!(
            "{} thing(s) are set to happen with it.\n",
            todo.hooks.len()
        ));
    }
    if todo.asked > 0 {
        notes.push_str(&format!(
            "You have asked about it {} time(s).\n",
            todo.asked
        ));
    }
    notes.push_str(&format!(
        "Made on {}. Its id is {}.\n",
        todo.created_at.format("%-d %B %Y"),
        todo.id
    ));
    ContextCard::new(kind.to_string(), todo.text.clone(), content, notes)
}

fn cart_card(item: &CartItem) -> ContextCard {
    let money = |amount: Option<f64>| {
        amount.map(|a| match item.currency.as_deref() {
            Some(currency) => format!("{currency} {a:.2}"),
            None => format!("{a:.2}"),
        })
    };
    let mut content = format!("{} ({})", item.site, item.url);
    if let Some(price) = money(item.price) {
        content.push_str(&format!("\nPrice now: {price}"));
    }
    match item.in_stock {
        Some(true) => content.push_str("\nIn stock."),
        Some(false) => content.push_str("\nOut of stock."),
        None => {}
    }
    let mut notes = String::new();
    for (label, amount) in [
        ("Before that", item.previous),
        ("Lowest seen", item.lowest),
        ("Highest seen", item.highest),
        ("Their target", item.target),
    ] {
        if let Some(amount) = money(amount) {
            notes.push_str(&format!("{label}: {amount}\n"));
        }
    }
    notes.push_str(&format!(
        "You check it every {} hour(s).\n",
        item.every_hours
    ));
    if let Some(checked) = item.checked_at {
        notes.push_str(&format!(
            "Last checked {} UTC.\n",
            checked.format("%-d %B %Y %H:%M")
        ));
    }
    if item.failures > 0 {
        notes.push_str(&format!("The last {} check(s) failed.\n", item.failures));
    }
    ContextCard::new(
        "Watched link".to_string(),
        item.title.clone(),
        content,
        notes,
    )
}

fn watch_card(watch: &WebWatch, changes: &[StoredChange]) -> ContextCard {
    let items = watch.items.as_deref().unwrap_or_default();
    let mut content = format!(
        "{}\nTells them about: {}",
        watch.address,
        watch.filters.words_for_people()
    );
    if let Some(standing) = watch.filters.tell_when.standing(items) {
        content.push_str(&format!("\nAt the last check {standing}."));
    }
    let mut notes = String::new();
    if !watch.looking_for.trim().is_empty() {
        notes.push_str(&format!(
            "What they asked for: {}\n",
            watch.looking_for.trim()
        ));
    }
    notes.push_str(&format!(
        "Checked every {} hour(s){}. The page listed {} thing(s) at the last check.\n",
        watch.interval_hours,
        if watch.paused { ", paused now" } else { "" },
        items.len()
    ));
    if watch.failures > 0 {
        notes.push_str(&format!("The last {} check(s) failed.\n", watch.failures));
    }
    for change in changes {
        notes.push_str(&format!(
            "{}: {}\n",
            change.found_at.format("%-d %B"),
            change.change.line()
        ));
    }
    notes.push_str(&format!("Its id is {}.\n", watch.id));
    ContextCard::new("Web watch".to_string(), watch.title.clone(), content, notes)
}

/// The member's card, or `None` when they have none of that kind by that id.
pub(crate) async fn resolve(
    state: &AppState,
    member: &str,
    card: &CardRef,
) -> anyhow::Result<Option<ContextCard>> {
    Ok(match card.kind.as_str() {
        "idea" => crate::ideas::repo(state)
            .get(member, &card.id)
            .await?
            .map(|idea| idea.as_card()),
        "news" => crate::news::store(state)
            .story(member, &card.id)
            .await?
            .map(|story| story.as_card()),
        "digest" => crate::news::store(state)
            .digest(member, &card.id)
            .await?
            .map(|digest| digest.as_card()),
        "todo" => crate::todos::repo(state)
            .get(member, &card.id)
            .await?
            .map(|todo| todo_card(&todo)),
        "cart" => crate::cart::repo(state)
            .get(member, &card.id)
            .await?
            .map(|item| cart_card(&item)),
        "watch" => {
            let watches = SqliteWebWatchRepository::new(state.db.system.clone());
            match watches.get(&card.id).await? {
                Some(watch) if watch.owner == member => {
                    let changes = watches.changes(&watch.id, WATCH_CHANGES).await?;
                    Some(watch_card(&watch, &changes))
                }
                _ => None,
            }
        }
        _ => None,
    })
}

/// `GET /me/cards/{kind}/{id}`: what a chat about this card would be given.
async fn read(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    if !KINDS.contains(&kind.as_str()) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            format!("kind must be one of: {}", KINDS.join(", ")),
        ));
    }
    let card = resolve(&state, &member, &CardRef { kind, id })
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such card of yours"))?;
    Ok(Json(json!(card)))
}
