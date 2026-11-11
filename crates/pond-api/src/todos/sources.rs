//! `GET /me/hooks/sources`: what a hook may listen to on this Pond, from what it really has, so
//! the phone's picker is data and a new connector needs no app release.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::{Extension, Json};
use serde_json::{json, Value};

use pond_adapters_vision::LiveCameraRegistry;
use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::todo::{HookCatalog, HookSource};
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::shared::ports::web_watch::WebWatchRepository;
use pond_core::user_data::domain::device::POWER_KEY;
use pond_core::user_data::domain::vision as vision_policy;
use pond_infra::sqlite_web_watch::SqliteWebWatchRepository;

use crate::me::{caller_member, server_error, Failure};
use crate::AppState;

/// One part of the list that could not be read leaves the rest of it.
fn or_none<T>(part: &str, read: anyhow::Result<Vec<T>>) -> Vec<T> {
    read.unwrap_or_else(|e| {
        tracing::warn!(part, error = %e, "[todos] a hook source could not be read");
        Vec::new()
    })
}

/// The member's places, and every source on this Pond a hook of theirs may name.
pub(crate) async fn hook_sources(state: &AppState, member: &str) -> Result<HookCatalog, Failure> {
    let repo = super::repo(state);
    let places = repo.places(member).await.map_err(server_error)?;
    let mut sources = vec![HookSource::presence()];

    // Cameras only while the household allows them: a hook on one would otherwise never fire.
    if let Ok(settings) = state.settings_repo.get().await {
        if settings.cameras_enabled {
            let mut cameras: BTreeSet<String> = LiveCameraRegistry::global()
                .active_cameras()
                .into_iter()
                .collect();
            cameras.insert(vision_policy::camera_id(&settings));
            sources.extend(cameras.iter().map(|id| HookSource::camera(id)));
        }
    }

    let devices = or_none("devices", state.device_registry.list_devices().await);
    sources.extend(
        devices
            .iter()
            .filter(|d| d.capabilities.iter().any(|c| c == POWER_KEY))
            .map(|d| HookSource::device(&d.id, &d.name)),
    );

    let watches = or_none(
        "web watches",
        SqliteWebWatchRepository::new(state.db.system.clone())
            .list_for(member)
            .await,
    );
    sources.extend(
        watches
            .iter()
            .map(|w| HookSource::web_watch(&w.id, &w.title, &w.address)),
    );
    sources.extend(or_none("cart", repo.cart_sources(member).await));
    sources.extend(or_none("connectors", repo.connector_sources(member).await));
    Ok(HookCatalog { places, sources })
}

pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let catalog = hook_sources(&state, &member).await?;
    Ok(Json(json!({
        "places": catalog.places.iter().map(|p| p.view()).collect::<Vec<_>>(),
        "sources": catalog.sources.iter().map(HookSource::view).collect::<Vec<_>>(),
    })))
}
