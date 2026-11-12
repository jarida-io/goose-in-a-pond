//! `/browser`: the headless browser the pond reads script-built pages with. The settings
//! screen shows whether one is there and whether a page could be rendered now, and downloads
//! the pond's own when the household asks.

use std::sync::Arc;

use axum::{http::StatusCode, routing::get, Json, Router};
use serde_json::{json, Value};

use pond_core::shared::ports::browser;

use crate::AppState;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/browser", get(status).delete(remove))
        .route("/browser/install", axum::routing::post(install))
}

fn not_set_up() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "reading pages with a browser is not set up on this pond"})),
    )
}

async fn status() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let browser = browser::installed().ok_or_else(not_set_up)?;
    Ok(Json(json!(browser.status())))
}

/// `POST /browser/install`: download the pond's own headless browser, about 130 MB. It runs
/// in the background; `GET /browser` reports how far it is.
async fn install() -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let browser = browser::installed().ok_or_else(not_set_up)?;
    let now = browser.status();
    if !now.can_download {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error": "no browser is published for this machine"})),
        ));
    }
    if now.own {
        return Ok((StatusCode::OK, Json(json!(now))));
    }
    tokio::spawn({
        let browser = browser.clone();
        async move {
            if let Err(why) = browser.install().await {
                tracing::warn!(target: "giap::trace", kind = "browser_install_failed", %why);
            }
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!(browser.status()))))
}

/// `DELETE /browser`: remove the pond's own browser. One installed on the machine stays.
async fn remove() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let browser = browser::installed().ok_or_else(not_set_up)?;
    browser.uninstall().await;
    Ok(Json(json!(browser.status())))
}
