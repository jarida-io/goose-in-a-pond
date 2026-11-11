//! Links a member asked Goose to watch the price of: the list, one link with its price history,
//! adding one (read in the background), changing what is watched for, removing one, and checking
//! one now. Each route reaches only the caller's own links.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::cart::{
    clamp_every_hours, number_in, CartItem, Notify, PricePoint, HISTORY_SHOWN,
};
use pond_core::shared::ports::cart::{CartRepository, ModelUse};
use pond_core::shared::services::cart_watch::{self, AddRefusal, CartWatch, NewCartItem};
use pond_infra::sqlite_cart::SqliteCartRepository;
use pond_infra::sqlite_hook_event_sink::SqliteHookEventSink;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// How long a "check now" waits for its answer; past it, the check finishes in the background.
const CHECK_NOW_WAIT: Duration = Duration::from_secs(25);

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/cart", get(list).post(add))
        .route("/me/cart/{id}", get(one).patch(edit).delete(remove))
        .route("/me/cart/{id}/check", post(check_now))
}

type Caller = Option<Extension<Principal>>;

fn repo(state: &AppState) -> Arc<dyn CartRepository> {
    Arc::new(SqliteCartRepository::new(state.db.system.clone()))
}

/// The cart over this request's database, with the reader and notifier the Pond installed.
fn watch(state: &AppState) -> Result<Arc<CartWatch>, Failure> {
    let installed = cart_watch::installed().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "watching prices is not set up on this Pond",
        )
    })?;
    Ok(Arc::new(installed.over(
        repo(state),
        Arc::new(SqliteHookEventSink::new(state.db.system.clone())),
    )))
}

fn money(amount: Option<f64>, currency: &Option<String>) -> Value {
    match amount {
        Some(amount) => json!({"amount": amount, "currency": currency.clone().unwrap_or_default()}),
        None => Value::Null,
    }
}

/// One link as the phone shows it: `CartItem` in `docs/architecture/my-goose-hub.md`.
pub(crate) fn item_view(item: &CartItem, history: &[PricePoint]) -> Value {
    json!({
        "id": item.id,
        "url": item.url,
        "title": item.title,
        "picture": item.picture_key.as_ref().map(|k| format!("/api/v1/report-images/{k}")),
        "site": item.site,
        "price": money(item.price, &item.currency),
        "previous": money(item.previous, &item.currency),
        "lowest": money(item.lowest, &item.currency),
        "highest": money(item.highest, &item.currency),
        "target": money(item.target, &item.currency),
        "in_stock": item.in_stock,
        "history": history
            .iter()
            .map(|p| json!({"at": p.at.to_rfc3339(), "amount": p.price}))
            .collect::<Vec<_>>(),
        "status": item.status.as_str(),
        "checked_at": item.checked_at.map(|t| t.to_rfc3339()),
        "next_check_at": item.next_check_at.map(|t| t.to_rfc3339()),
        "every_hours": item.every_hours,
        "collection": item.collection,
        "notify": item.notify.as_str(),
    })
}

async fn view(repo: &dyn CartRepository, item: &CartItem) -> Result<Value, Failure> {
    let history = repo
        .history(&item.id, HISTORY_SHOWN)
        .await
        .map_err(server_error)?;
    Ok(item_view(item, &history))
}

async fn owned(state: &AppState, member: &str, id: &str) -> Result<CartItem, Failure> {
    repo(state)
        .get(member, id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such link in your cart"))
}

/// A target as a member may write it: a number, "1.2M", or a `Money`.
fn amount_of(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => number_in(s),
        Value::Object(o) => o.get("amount").and_then(amount_of),
        _ => None,
    }
    .filter(|a| a.is_finite() && *a > 0.0)
}

fn notify_of(value: Option<&str>) -> Result<Option<Notify>, Failure> {
    match value {
        None => Ok(None),
        Some(text) => Notify::parse(text).map(Some).ok_or_else(|| {
            failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "notify is one of drop, target, stock or any",
            )
        }),
    }
}

/// `GET /me/cart` → `{"items": [CartItem]}`, newest first.
async fn list(
    State(state): State<Arc<AppState>>,
    principal: Caller,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let repo = repo(&state);
    let items = repo.list_for(&member).await.map_err(server_error)?;
    let mut views = Vec::with_capacity(items.len());
    for item in &items {
        views.push(view(repo.as_ref(), item).await?);
    }
    Ok(Json(json!({ "items": views })))
}

#[derive(Debug, Deserialize)]
struct AddBody {
    url: String,
    #[serde(default)]
    target: Option<Value>,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    every_hours: Option<f64>,
    #[serde(default)]
    notify: Option<String>,
}

/// `POST /me/cart` → 201 with the item, `checking`: its first read runs in the background.
async fn add(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    Json(body): Json<AddBody>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let notify = notify_of(body.notify.as_deref())?;
    let watch = watch(&state)?;
    let now = Utc::now();
    let new = NewCartItem {
        url: body.url,
        target: body.target.as_ref().and_then(amount_of),
        collection: body.collection,
        every_hours: body.every_hours,
        notify,
    };
    let item = match watch.add(&member, new, now).await.map_err(server_error)? {
        Ok(item) => item,
        Err(AddRefusal::NotAnAddress) => {
            return Err(failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "that is not a web address",
            ))
        }
        Err(AddRefusal::TooMany) => {
            return Err(failure(
                StatusCode::CONFLICT,
                "your cart is full; remove a link before adding another",
            ))
        }
        Err(AddRefusal::AlreadyWatched(existing)) => {
            let existing = view(watch.repo().as_ref(), &existing).await?;
            return Err((
                StatusCode::CONFLICT,
                Json(json!({"error": "that link is already in your cart", "item": existing})),
            ));
        }
    };
    let first_read = watch.clone();
    let unread = item.clone();
    tokio::spawn(async move {
        if let Err(e) = first_read
            .check(unread, ModelUse::Allowed, Utc::now())
            .await
        {
            tracing::warn!(target: "giap::trace", kind = "cart_first_check_failed", error = %e);
        }
    });
    Ok((StatusCode::CREATED, Json(item_view(&item, &[]))))
}

/// `GET /me/cart/{id}` → CartItem.
async fn one(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let item = owned(&state, &member, &id).await?;
    Ok(Json(view(repo(&state).as_ref(), &item).await?))
}

/// `PATCH /me/cart/{id}`: `target` (null clears it), `collection` (null clears it),
/// `every_hours` and `notify` → CartItem.
async fn edit(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let mut item = owned(&state, &member, &id).await?;
    let Some(fields) = body.as_object() else {
        return Err(failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "send the fields to change",
        ));
    };
    if let Some(target) = fields.get("target") {
        item.target = match target {
            Value::Null => None,
            other => Some(amount_of(other).ok_or_else(|| {
                failure(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "target is a price above zero",
                )
            })?),
        };
    }
    if let Some(collection) = fields.get("collection") {
        item.collection = collection
            .as_str()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
    }
    if let Some(hours) = fields.get("every_hours") {
        let hours = hours.as_f64().ok_or_else(|| {
            failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "every_hours is a number of hours",
            )
        })?;
        item.every_hours = clamp_every_hours(hours);
        let from = item.checked_at.unwrap_or_else(Utc::now);
        item.next_check_at = Some(from + chrono::Duration::hours(item.every_hours.into()));
    }
    if let Some(notify) = notify_of(fields.get("notify").and_then(Value::as_str))? {
        item.notify = notify;
    }
    item.updated_at = Utc::now();
    let repo = repo(&state);
    repo.save(&item).await.map_err(server_error)?;
    Ok(Json(view(repo.as_ref(), &item).await?))
}

/// `DELETE /me/cart/{id}` → 204.
async fn remove(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    if repo(&state)
        .delete(&member, &id)
        .await
        .map_err(server_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(failure(StatusCode::NOT_FOUND, "no such link in your cart"))
    }
}

/// `POST /me/cart/{id}/check` → CartItem, read now. A read past [`CHECK_NOW_WAIT`] carries on in
/// the background, and the item comes back as it stands.
async fn check_now(
    State(state): State<Arc<AppState>>,
    principal: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let item = owned(&state, &member, &id).await?;
    let watch = watch(&state)?;
    let checking = {
        let watch = watch.clone();
        let item = item.clone();
        tokio::spawn(async move { watch.check(item, ModelUse::Allowed, Utc::now()).await })
    };
    let item = match tokio::time::timeout(CHECK_NOW_WAIT, checking).await {
        Ok(Ok(Ok(checked))) => checked,
        Ok(Ok(Err(e))) => return Err(server_error(e)),
        _ => owned(&state, &member, &id).await?,
    };
    Ok(Json(view(watch.repo().as_ref(), &item).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::app_state;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use pond_core::shared::domain::cart::{ProductReading, ReadBy};
    use pond_core::shared::ports::cart::{
        CartNotice, CartNotifier, ProductReadError, ProductReader,
    };
    use pond_core::shared::ports::hook_event_sink::{HookEventSink, NewHookEvent};
    use tower::ServiceExt;

    struct Kettle;

    #[async_trait]
    impl ProductReader for Kettle {
        async fn read(
            &self,
            _url: &str,
            _model: ModelUse,
        ) -> Result<ProductReading, ProductReadError> {
            Ok(ProductReading {
                title: Some("Ramtons Cordless Electric Kettle".into()),
                site: Some("Jumia Kenya".into()),
                price: Some(1799.0),
                currency: Some("KES".into()),
                in_stock: Some(true),
                read_by: Some(ReadBy::JsonLd),
                ..Default::default()
            })
        }
    }

    struct Quiet;

    #[async_trait]
    impl CartNotifier for Quiet {
        async fn notify(&self, _notice: CartNotice) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl HookEventSink for Quiet {
        async fn record(&self, _event: NewHookEvent) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    async fn call(router: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn a_link_is_added_checked_edited_and_removed_by_its_owner() {
        let test = app_state().await;
        // The installed cart lends only its reader and notifier; each route brings its own store.
        cart_watch::install(Arc::new(CartWatch::new(
            repo(&test.state),
            Arc::new(Kettle),
            Arc::new(Quiet),
            Arc::new(Quiet),
        )));
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('jerry', 'Jerry')")
            .execute(&test.state.db.system)
            .await
            .unwrap();
        let router = routes().with_state(Arc::new(test.state));

        let (status, added) = call(
            &router,
            "POST",
            "/me/cart",
            json!({"url": "jumia.co.ke/kettle-177871891.html", "target": "1,500", "notify": "target"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        assert_eq!(added["status"], "checking");
        assert_eq!(added["target"]["amount"], 1500.0);
        assert_eq!(added["notify"], "target");
        let id = added["id"].as_str().unwrap().to_string();

        let (status, again) = call(
            &router,
            "POST",
            "/me/cart",
            json!({"url": "https://jumia.co.ke/kettle-177871891.html"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(again["item"]["id"], id.as_str());

        // The first read runs in the background; a check made while it runs answers the item
        // as it stands.
        for _ in 0..100 {
            let (_, item) = call(&router, "GET", &format!("/me/cart/{id}"), Value::Null).await;
            if item["status"] == "ok" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (status, checked) =
            call(&router, "POST", &format!("/me/cart/{id}/check"), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{checked}");
        assert_eq!(checked["status"], "ok");
        assert_eq!(
            checked["price"],
            json!({"amount": 1799.0, "currency": "KES"})
        );
        assert_eq!(checked["lowest"]["amount"], 1799.0);
        assert_eq!(checked["in_stock"], true);
        assert_eq!(checked["history"].as_array().unwrap().len(), 1);
        assert_eq!(checked["title"], "Ramtons Cordless Electric Kettle");

        let (status, edited) = call(
            &router,
            "PATCH",
            &format!("/me/cart/{id}"),
            json!({"target": null, "every_hours": 1000, "notify": "any", "collection": "Kitchen"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["target"], Value::Null);
        assert_eq!(edited["every_hours"], 168);
        assert_eq!(edited["notify"], "any");
        assert_eq!(edited["collection"], "Kitchen");
        let (status, _) = call(
            &router,
            "PATCH",
            &format!("/me/cart/{id}"),
            json!({"notify": "soon"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let (status, listed) = call(&router, "GET", "/me/cart", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["items"].as_array().unwrap().len(), 1);

        let (status, _) = call(&router, "DELETE", &format!("/me/cart/{id}"), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&router, "GET", &format!("/me/cart/{id}"), Value::Null).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
