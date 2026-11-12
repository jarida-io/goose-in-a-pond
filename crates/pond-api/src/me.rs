//! My Goose: the routes that act for the calling member, under `/me`. Each part keeps its own
//! module and router; this one resolves who is calling and says which parts this Pond has.

use std::sync::Arc;

use axum::{http::StatusCode, routing::get, Extension, Json, Router};
use serde_json::{json, Value};

use pond_core::security::domain::proven_device::{DeviceRung, ProvenDevice};
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;

use crate::AppState;

pub(crate) type Failure = (StatusCode, Json<Value>);

pub(crate) fn failure(status: StatusCode, error: impl Into<String>) -> Failure {
    (status, Json(json!({"error": error.into()})))
}

pub(crate) fn server_error(e: impl std::fmt::Display) -> Failure {
    failure(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// The member a `/me` call acts for: the calling device's member, else the household's only one.
/// Anything else is refused rather than guessed, so one member never reads another's lists.
pub(crate) async fn caller_member(
    state: &AppState,
    principal: Option<&Extension<Principal>>,
) -> Result<String, Failure> {
    let device = match principal {
        Some(Extension(principal)) => ProvenDevice::from_principal(principal),
        None => ProvenDevice::none(),
    };
    let rung = match device.id() {
        None => DeviceRung::NoDevice,
        Some(id) => device.rung(
            pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
                state.db.system.clone(),
            )
            .device_profile(id)
            .await,
        ),
    };
    match rung {
        DeviceRung::Member(id) => Ok(id),
        DeviceRung::NoDevice | DeviceRung::Unattributed => crate::member_notes::sole_member(state)
            .await
            .ok_or_else(|| {
                failure(
                    StatusCode::CONFLICT,
                    "who_are_you: this phone is not linked to a member yet",
                )
            }),
        DeviceRung::Unavailable(e) => Err(server_error(e)),
    }
}

/// Which parts of My Goose this Pond serves; a Pond without this route has none of them.
async fn hub() -> Json<Value> {
    Json(json!({
        "version": 1,
        "about": crate::about_me::SERVED,
        "interests": crate::interests::SERVED,
        "interview": true,
        "news": crate::news::SERVED,
        "todos": crate::todos::SERVED,
        "cart": crate::cart::SERVED,
        "connectors": crate::connectors::SERVED,
        "ideas": crate::ideas::SERVED,
        "screenshots": true,
        "catalog": crate::catalog::SERVED,
        "cards": crate::context_cards::KINDS,
    }))
}

/// Merged into the protected router, so every `/me` route needs a paired phone.
pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/hub", get(hub))
        .merge(crate::about_me::routes())
        .merge(crate::agent_notes::routes())
        .merge(crate::interests::routes())
        .merge(crate::member_interview::routes())
        .merge(crate::news::routes())
        .merge(crate::todos::routes())
        .merge(crate::cart::routes())
        .merge(crate::connectors::routes())
        .merge(crate::context_cards::routes())
        .merge(crate::ideas::routes())
        .merge(crate::screenshots::routes())
        .merge(crate::catalog::routes())
}
