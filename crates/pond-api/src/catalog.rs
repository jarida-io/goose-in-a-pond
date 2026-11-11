//! What else Goose can do on this Pond, and whether each thing is on.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use pond_core::mcp::domain::web_search::BuiltInSearchState;
use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::catalog::{catalog, CatalogInputs, Served};
use pond_core::shared::domain::connector::built_in_manifests;
use pond_core::shared::services::connector_hub;

use crate::me::{caller_member, Failure};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/me/catalog", get(list))
}

/// `GET /me/catalog` → `{"items": [CatalogItem]}`, read from the settings and the connectors.
async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let world = crate::connectors::world(&state, &member).await?;
    let connectors = connector_hub::connectors(&built_in_manifests(), &world);
    let items = catalog(&CatalogInputs {
        settings: &world.settings,
        connectors: &connectors,
        served: Served {
            interests: crate::interests::SERVED,
            news: crate::news::SERVED,
            todos: crate::todos::SERVED,
            cart: crate::cart::SERVED,
            ideas: crate::ideas::SERVED,
        },
        web_search: world.web_search == BuiltInSearchState::On,
        voice_notes: state.transcribe_audio.is_some(),
    });
    Ok(Json(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::app_state;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
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
    async fn the_catalogue_and_the_connectors_answer_for_the_only_member() {
        let test = app_state().await;
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('jerry', 'Jerry')")
            .execute(&test.state.db.system)
            .await
            .unwrap();
        let router = routes()
            .merge(crate::connectors::routes())
            .with_state(Arc::new(test.state));
        let (status, body) = get_json(&router, "/me/catalog").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let items = body["items"].as_array().unwrap();
        let cart = items.iter().find(|i| i["id"] == "cart").unwrap();
        assert_eq!(cart["state"], "on");
        assert_eq!(cart["opens"], "cart");
        let news = items.iter().find(|i| i["id"] == "news").unwrap();
        assert_eq!(
            news["state"],
            if crate::news::SERVED {
                "needs_setup"
            } else {
                "off"
            }
        );
        let attachments = items.iter().find(|i| i["id"] == "attachments").unwrap();
        assert_eq!(
            attachments["state"], "needs_setup",
            "no speech model in a test"
        );

        let (status, body) = get_json(&router, "/me/connectors").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let connectors = body["connectors"].as_array().unwrap();
        let mail = connectors.iter().find(|c| c["id"] == "mail").unwrap();
        assert_eq!(mail["status"], "error", "a test Pond keeps no passwords");
        assert_eq!(mail["setup"]["fields"][0]["kind"], "select");
        assert_eq!(mail["provides"]["events"][0]["id"], "new_mail");
        let webhook = connectors.iter().find(|c| c["id"] == "webhook").unwrap();
        assert_eq!(webhook["kind"], "webhook");
        assert_eq!(webhook["status"], "available");
    }
}
