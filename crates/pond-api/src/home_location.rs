//! `PUT /api/v1/location/device`: a household phone tells the Pond where home is, from home.
//!
//! A Pond has no GPS. Without a typed place its location is the time zone's city, and a typed
//! place is geocoded to that town's centre: a household in Ndenderu got Nairobi's weather. A
//! paired phone has a real fix, so it may send one, under three rules:
//!
//! - only from the household network: a request that reached the Pond over the tailnet, or from
//!   any address outside the Pond's own LAN, is refused `not_on_home_network`, since a phone away
//!   from home would move the household to wherever its owner is;
//! - only while the Pond's location is still a guess (`Settings::weather_location_is_precise`),
//!   unless the person asked for it (`replace`), so a place someone entered is not overwritten;
//! - rounded to three decimal places (about 110 m): enough for weather and local results, without
//!   keeping the house's exact spot.
//!
//! The phone names the place itself (its system geocoder), because the Pond's geocoder only goes
//! from names to coordinates; without a name the Pond keeps the one it had.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::rejection::ExtensionRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use pond_core::security::ports::remote_access::TailnetMachine;
use pond_core::user_data::domain::settings::WEATHER_LOCATION_DEVICE;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

/// A fix vaguer than this says where the town is, not the house: refused rather than stored.
pub const MAX_ACCURACY_M: f64 = 2_000.0;

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, error: &str) -> Refusal {
    (status, Json(json!({"error": error})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceLocation {
    latitude: f64,
    longitude: f64,
    /// The fix's horizontal accuracy in metres, as the phone reported it.
    accuracy_m: f64,
    /// The place's name as the phone's geocoder gave it ("Ndenderu"), when it gave one.
    #[serde(default)]
    name: Option<String>,
    /// The person asked for this phone's location; otherwise only a guess is replaced.
    #[serde(default)]
    replace: bool,
}

/// To three decimal places, about 110 m on the ground.
fn rounded(degrees: f64) -> f64 {
    (degrees * 1_000.0).round() / 1_000.0
}

/// `PUT /api/v1/location/device`
///
/// **Request**: `{latitude, longitude, accuracy_m, name?, replace?}` from a paired device on the
/// household LAN.
///
/// **Response**: `{applied, reason?, name, latitude, longitude}`. `applied` is false with
/// `reason: "location_known"` when the Pond already has a position someone set or a phone sent
/// and `replace` was not asked for. 403 `not_on_home_network` from anywhere but the LAN; 422
/// `invalid_coordinates` or `too_imprecise` for a fix not worth keeping.
pub(crate) async fn set_from_device(
    State(state): State<Arc<AppState>>,
    peer: Result<ConnectInfo<SocketAddr>, ExtensionRejection>,
    tailnet: Result<Extension<TailnetMachine>, ExtensionRejection>,
    Json(fix): Json<DeviceLocation>,
) -> Result<Json<Value>, Refusal> {
    // The tailnet's peer address is never on the LAN, but the marker says so outright.
    if tailnet.is_ok() || crate::network::require_lan(peer.ok()).is_err() {
        tracing::info!(
            kind = "location_from_phone",
            applied = false,
            reason = "not_on_home_network"
        );
        return Err(refuse(StatusCode::FORBIDDEN, "not_on_home_network"));
    }
    let valid = fix.latitude.is_finite()
        && fix.longitude.is_finite()
        && (-90.0..=90.0).contains(&fix.latitude)
        && (-180.0..=180.0).contains(&fix.longitude)
        && (fix.latitude != 0.0 || fix.longitude != 0.0);
    if !valid {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_coordinates",
        ));
    }
    if !(fix.accuracy_m.is_finite() && fix.accuracy_m >= 0.0 && fix.accuracy_m <= MAX_ACCURACY_M) {
        return Err(refuse(StatusCode::UNPROCESSABLE_ENTITY, "too_imprecise"));
    }

    let failed = |e: anyhow::Error| {
        tracing::warn!(kind = "location_from_phone", error = %e, "could not save the location");
        refuse(StatusCode::INTERNAL_SERVER_ERROR, "location_not_saved")
    };
    let mut settings = state.settings_repo.get().await.map_err(failed)?;
    if settings.weather_location_is_precise() && !fix.replace {
        tracing::info!(
            kind = "location_from_phone",
            applied = false,
            reason = "location_known"
        );
        return Ok(Json(json!({
            "applied": false,
            "reason": "location_known",
            "name": settings.weather_location_name,
            "latitude": settings.weather_latitude,
            "longitude": settings.weather_longitude,
        })));
    }

    settings.weather_latitude = rounded(fix.latitude);
    settings.weather_longitude = rounded(fix.longitude);
    settings.weather_location_source = WEATHER_LOCATION_DEVICE.to_string();
    if let Some(name) = fix.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        settings.weather_location_name = name.chars().take(120).collect();
    }
    let keys: std::collections::HashSet<String> = [
        "weather_latitude",
        "weather_longitude",
        "weather_location_source",
        "weather_location_name",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    state
        .settings_repo
        .update_fields(&settings, Some(&keys))
        .await
        .map_err(failed)?;
    // The place's name only: the household's coordinates stay out of the logs.
    tracing::info!(
        kind = "location_from_phone",
        applied = true,
        replace = fix.replace,
        place = %settings.weather_location_name,
        "the Pond's location is now a household phone's, from the home network"
    );
    Ok(Json(json!({
        "applied": true,
        "name": settings.weather_location_name,
        "latitude": settings.weather_latitude,
        "longitude": settings.weather_longitude,
    })))
}
