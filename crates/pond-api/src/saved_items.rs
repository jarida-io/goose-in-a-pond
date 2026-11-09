//! A member's saved cards for their screens: list and collections, keep one (such as an option a
//! task found), edit it or share it with the household, delete it, and watch a saved listing's
//! price. A member sees their own cards and the household's shared ones, and changes only their own.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use pond_core::security::ports::policy::Principal;
use pond_core::security::ports::remote_access::TailnetMachine;
use pond_core::shared::domain::saved_item::{
    SavedFields, SavedItem, SavedSource, MAX_SAVED_PER_MEMBER,
};
use pond_core::shared::domain::web_watch::{
    WatchFilters, DEFAULT_INTERVAL_HOURS, MAX_WATCHES_PER_MEMBER,
};
use pond_core::shared::ports::saved_item::{SavedEdit, SavedItemRepository};
use pond_core::shared::ports::web_watch::{WebWatch, WebWatchRepository};
use pond_infra::sqlite_saved_item::SqliteSavedItemRepository;
use pond_infra::sqlite_web_watch::SqliteWebWatchRepository;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

type Peer = Result<
    axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::rejection::ExtensionRejection,
>;
type Insecure = Option<Extension<crate::insecure_dev::InsecureDevTransport>>;

fn repo(state: &Arc<AppState>) -> SqliteSavedItemRepository {
    SqliteSavedItemRepository::new(state.db.system.clone())
}

async fn caller(
    state: &Arc<AppState>,
    principal: Option<&Extension<Principal>>,
    peer: Peer,
    insecure: bool,
    tailnet: bool,
) -> Option<String> {
    crate::routes::long_task_caller(state, principal, peer, insecure, tailnet).await
}

fn answer(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({"error": error}))).into_response()
}

fn failed(e: anyhow::Error) -> Response {
    answer(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
}

fn unknown_caller() -> Response {
    answer(
        StatusCode::FORBIDDEN,
        "saved cards belong to a household member; this device is not one",
    )
}

/// One card as a screen shows it; `mine` says whether the caller may change it.
fn card_view(item: &SavedItem, member: &str) -> Value {
    let mut view = serde_json::to_value(item).unwrap_or(Value::Null);
    view["mine"] = json!(item.owner == member);
    view
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub collection: Option<String>,
    pub kind: Option<String>,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return Json(json!([])).into_response();
    };
    match repo(&state)
        .list_visible(&member, query.collection.as_deref(), query.kind.as_deref())
        .await
    {
        Ok(items) => Json(Value::Array(
            items.iter().map(|i| card_view(i, &member)).collect(),
        ))
        .into_response(),
        Err(e) => failed(e),
    }
}

pub async fn collections(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return Json(json!([])).into_response();
    };
    match repo(&state).collections(&member).await {
        Ok(found) => Json(Value::Array(
            found
                .into_iter()
                .map(|(name, count)| json!({"name": name, "count": count}))
                .collect(),
        ))
        .into_response(),
        Err(e) => failed(e),
    }
}

/// A card to keep, as a screen sends it: the kind's own fields beside the common ones.
#[derive(Debug, Deserialize)]
pub struct NewCard {
    pub title: String,
    #[serde(flatten)]
    pub fields: SavedFields,
    #[serde(default)]
    pub body: String,
    pub collection: Option<String>,
    #[serde(default)]
    pub sources: Vec<SavedSource>,
    pub session_id: Option<String>,
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Json(card): Json<NewCard>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    let repo = repo(&state);
    match repo.count_for(&member).await {
        Ok(n) if n >= MAX_SAVED_PER_MEMBER => {
            return answer(StatusCode::CONFLICT, "the most cards one member may keep");
        }
        Ok(_) => {}
        Err(e) => return failed(e),
    }
    let item = match SavedItem::new(
        uuid::Uuid::new_v4().to_string(),
        member.clone(),
        &card.title,
        card.fields,
        &card.body,
        card.collection.as_deref(),
        card.sources,
        card.session_id,
        Utc::now(),
    ) {
        Ok(item) => item,
        Err(refused) => return answer(StatusCode::BAD_REQUEST, &refused.words()),
    };
    if let Err(e) = repo.create(&item).await {
        return failed(e);
    }
    match repo.get(&item.id).await {
        Ok(Some(stored)) => {
            (StatusCode::CREATED, Json(card_view(&stored, &member))).into_response()
        }
        Ok(None) => answer(StatusCode::INTERNAL_SERVER_ERROR, "the card was not kept"),
        Err(e) => failed(e),
    }
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    match repo(&state).get(&id).await {
        Ok(Some(item)) if item.visible_to(&member) => {
            Json(card_view(&item, &member)).into_response()
        }
        Ok(_) => answer(StatusCode::NOT_FOUND, "no such card"),
        Err(e) => failed(e),
    }
}

/// Absent keeps the collection; `null` takes the card out of it.
fn unchanged() -> Value {
    Value::Bool(false)
}

#[derive(Debug, Deserialize)]
pub struct CardEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    #[serde(default = "unchanged")]
    pub collection: Value,
    pub shared: Option<bool>,
}

pub async fn edit(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
    Json(body): Json<CardEdit>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    let collection = match body.collection {
        Value::Bool(false) => None,
        Value::Null => Some(None),
        Value::String(name) => Some(Some(name)),
        _ => return answer(StatusCode::BAD_REQUEST, "collection is a name, or null"),
    };
    let edit = SavedEdit {
        title: body.title,
        body: body.body,
        collection,
        shared: body.shared,
    };
    let repo = repo(&state);
    match repo.edit(&member, &id, &edit).await {
        Ok(true) => match repo.get(&id).await {
            Ok(Some(item)) => Json(card_view(&item, &member)).into_response(),
            Ok(None) => answer(StatusCode::NOT_FOUND, "no such card"),
            Err(e) => failed(e),
        },
        Ok(false) => answer(StatusCode::NOT_FOUND, "no card of theirs by that id"),
        Err(e) => failed(e),
    }
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
) -> Response {
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    match repo(&state).delete(&member, &id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => answer(StatusCode::NOT_FOUND, "no card of theirs by that id"),
        Err(e) => failed(e),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct WatchBody {
    pub max_price: Option<f64>,
    pub every_hours: Option<u32>,
}

/// "Watch price": a watch on the saved listing's page, which tells the caller when its price
/// drops. The first check reads the page and keeps its items; nothing is reported until a later one.
pub async fn watch(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    peer: Peer,
    insecure: Insecure,
    tailnet: Option<Extension<TailnetMachine>>,
    Path(id): Path<String>,
    body: Option<Json<WatchBody>>,
) -> Response {
    use pond_core::shared::domain::web_watch::{MAX_INTERVAL_HOURS, MIN_INTERVAL_HOURS};
    let Some(member) = caller(
        &state,
        principal.as_ref(),
        peer,
        insecure.is_some(),
        tailnet.is_some(),
    )
    .await
    else {
        return unknown_caller();
    };
    let item = match repo(&state).get(&id).await {
        Ok(Some(item)) if item.visible_to(&member) => item,
        Ok(_) => return answer(StatusCode::NOT_FOUND, "no such card"),
        Err(e) => return failed(e),
    };
    let Some(link) = item.fields.link().map(str::to_string) else {
        return answer(
            StatusCode::BAD_REQUEST,
            "only a listing or a place with a link can be watched",
        );
    };
    let watches = SqliteWebWatchRepository::new(state.db.system.clone());
    let mine = match watches.list_for(&member).await {
        Ok(mine) => mine,
        Err(e) => return failed(e),
    };
    if let Some(existing) = mine.iter().find(|w| w.address == link) {
        return (
            StatusCode::OK,
            Json(crate::web_watches::watch_view(existing)),
        )
            .into_response();
    }
    if mine.len() >= MAX_WATCHES_PER_MEMBER {
        return answer(StatusCode::CONFLICT, "the most watches one member may keep");
    }
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let now = Utc::now();
    let watch = WebWatch {
        id: uuid::Uuid::new_v4().to_string(),
        owner: member,
        address: link,
        title: item.title.clone(),
        looking_for: format!("The price of {}", item.title),
        filters: WatchFilters {
            max_price: body.max_price.filter(|p| p.is_finite() && *p > 0.0),
            ..Default::default()
        },
        interval_hours: body
            .every_hours
            .unwrap_or(DEFAULT_INTERVAL_HOURS)
            .clamp(MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS),
        paused: false,
        items: None,
        failures: 0,
        last_checked_at: None,
        next_check_at: now,
        created_at: now,
        session_id: item.session_id.clone(),
    };
    match watches.create(&watch).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(crate::web_watches::watch_view(&watch)),
        )
            .into_response(),
        Err(e) => failed(e),
    }
}
