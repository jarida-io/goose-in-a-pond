//! A member's own phone books a ride: get a fare from where it is, confirm or decline it, follow
//! it, cancel it. The member is the one the paired phone belongs to, never a request field.

use std::sync::{Arc, OnceLock};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use pond_core::rides::booking::{BookingError, RideBooking};
use pond_core::rides::domain::{BookingState, PendingRide, Place};
use pond_core::security::domain::proven_device::{DeviceRung, ProvenDevice};
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

type Refusal = (StatusCode, Json<Value>);

static BOOKING: OnceLock<Arc<RideBooking>> = OnceLock::new();

/// Install ride booking. Call once at startup, when a ride provider is configured.
pub fn install(booking: Arc<RideBooking>) {
    let _ = BOOKING.set(booking);
}

fn refuse(status: StatusCode, message: &str) -> Refusal {
    (status, Json(json!({ "error": message })))
}

fn booking() -> Result<&'static Arc<RideBooking>, Refusal> {
    BOOKING.get().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "ride booking is not set up on this pond",
        )
    })
}

/// Booking, for a new fare or a confirm: only while the household has travel switched on.
/// Reading, declining and cancelling a ride already quoted or booked still work when it is off.
async fn booking_for_new_rides(state: &AppState) -> Result<&'static Arc<RideBooking>, Refusal> {
    let booking = booking()?;
    match state.settings_repo.get().await {
        Ok(settings) if settings.ext_travel_enabled => Ok(booking),
        Ok(_) => Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "travel is switched off on this pond",
        )),
        Err(e) => {
            tracing::warn!(error = %e, "rides: could not read whether travel is on");
            Err(refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not tell whether travel is on",
            ))
        }
    }
}

/// The member this request's paired phone belongs to.
async fn member(state: &AppState, principal: Option<&Principal>) -> Result<String, Refusal> {
    let device = principal.map_or_else(ProvenDevice::none, ProvenDevice::from_principal);
    let attribution = match device.id() {
        Some(id) => {
            pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
                state.db.system.clone(),
            )
            .device_profile(id)
            .await
        }
        None => Ok(None),
    };
    match device.rung(attribution) {
        DeviceRung::Member(profile_id) => Ok(profile_id),
        DeviceRung::NoDevice => Err(refuse(
            StatusCode::FORBIDDEN,
            "rides are booked from the member's own paired phone",
        )),
        DeviceRung::Unattributed => Err(refuse(
            StatusCode::FORBIDDEN,
            "this phone is not linked to a household member",
        )),
        DeviceRung::Unavailable(why) => {
            tracing::warn!(error = %why, "rides: could not read the phone's member");
            Err(refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not tell whose phone this is",
            ))
        }
    }
}

/// Another member's ride reads as missing: it is no business of this phone that it exists.
fn booking_refusal(error: BookingError) -> Refusal {
    match error {
        BookingError::NotFound | BookingError::NotYours => {
            refuse(StatusCode::NOT_FOUND, "no such ride")
        }
        BookingError::Expired => refuse(StatusCode::GONE, &error.to_string()),
        BookingError::AlreadyDecided(_) | BookingError::NotRequested => {
            refuse(StatusCode::CONFLICT, &error.to_string())
        }
        BookingError::TooFar(_) => refuse(StatusCode::UNPROCESSABLE_ENTITY, &error.to_string()),
        BookingError::Provider(e) => {
            tracing::warn!(error = %format!("{e:#}"), "rides: the ride company refused");
            refuse(StatusCode::BAD_GATEWAY, &format!("{e:#}"))
        }
    }
}

#[derive(Deserialize)]
pub struct Point {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct QuoteRequest {
    pub pickup: Point,
    pub dropoff: Point,
}

fn place(point: Point, fallback_name: &str) -> Result<Place, Refusal> {
    let on_earth = (-90.0..=90.0).contains(&point.latitude)
        && (-180.0..=180.0).contains(&point.longitude)
        && !(point.latitude == 0.0 && point.longitude == 0.0);
    if !on_earth {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            "that is not a place on the map",
        ));
    }
    let name = point
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback_name.to_string());
    Ok(Place {
        name,
        latitude: point.latitude,
        longitude: point.longitude,
    })
}

/// A trip found under way rather than quoted here has no pickup, drop-off or fare to show.
fn view(pending: &PendingRide, provider: &str) -> Value {
    let trip = pending.trip.as_ref();
    json!({
        "id": pending.id,
        "provider": provider,
        "pickup": trip.map(|t| &t.pickup),
        "dropoff": trip.map(|t| &t.dropoff),
        "fare": trip.map(|t| json!({
            "display": t.quote.display,
            "currency_code": t.quote.currency_code,
            "expires_at": t.quote.expires_at,
        })),
        "pickup_eta_mins": trip.and_then(|t| t.quote.pickup_eta_mins),
        "state": pending.state,
    })
}

/// `POST /api/v1/rides/quote` — an upfront fare from the phone's location. Books nothing.
pub async fn quote(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(request): Json<QuoteRequest>,
) -> Result<Json<Value>, Refusal> {
    let profile_id = member(&state, principal.as_deref()).await?;
    let booking = booking_for_new_rides(&state).await?;
    let pickup = place(request.pickup, "Pickup")?;
    let dropoff = place(request.dropoff, "Destination")?;
    let pending = booking
        .quote(&profile_id, pickup, dropoff, chrono::Utc::now())
        .await
        .map_err(booking_refusal)?;
    Ok(Json(view(&pending, booking.provider_name())))
}

/// `GET /api/v1/rides/{id}`
pub async fn get(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    let profile_id = member(&state, principal.as_deref()).await?;
    let booking = booking()?;
    let pending = booking.get(&id, &profile_id).map_err(booking_refusal)?;
    Ok(Json(view(&pending, booking.provider_name())))
}

/// `POST /api/v1/rides/{id}/confirm` — the member's yes; the ride is requested now. 202 when the
/// company's answer was lost: it may have booked the ride, and the pond keeps asking.
pub async fn confirm(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), Refusal> {
    let profile_id = member(&state, principal.as_deref()).await?;
    let booking = booking_for_new_rides(&state).await?;
    let outcome = booking
        .confirm(&id, &profile_id, chrono::Utc::now())
        .await
        .map_err(booking_refusal)?;
    let pending = booking.get(&id, &profile_id).map_err(booking_refusal)?;
    let mut body = view(&pending, booking.provider_name());
    if matches!(outcome, BookingState::OutcomeUnknown { .. }) {
        body["message"] = json!(
            "The ride company did not answer the booking and may have booked the ride: check \
             its app. The pond keeps checking."
        );
        return Ok((StatusCode::ACCEPTED, Json(body)));
    }
    Ok((StatusCode::OK, Json(body)))
}

/// `POST /api/v1/rides/{id}/decline`
pub async fn decline(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Refusal> {
    let profile_id = member(&state, principal.as_deref()).await?;
    booking()?
        .decline(&id, &profile_id)
        .await
        .map_err(booking_refusal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/rides/{id}/cancel` — cancel a booked ride. The company may charge a fee.
pub async fn cancel(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Refusal> {
    let profile_id = member(&state, principal.as_deref()).await?;
    booking()?
        .cancel(&id, &profile_id)
        .await
        .map_err(booking_refusal)?;
    Ok(StatusCode::NO_CONTENT)
}
