//! The pairing lock's routes. Two are for a phone that is not yet paired and came back for its
//! request (`/handshake/pairing-request/*`, from the home network only). The rest are for a
//! paired member: reading the lock, and, for the main member or whoever has My Pond unlocked,
//! changing it and answering the phones that wait.

use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use pond_core::mcp::ports::notification::{Notification, OpenScreen};
use pond_core::security::domain::pairing_lock::{
    pin_is_acceptable, PairingClaim, PinCheck, MIN_PIN_DIGITS,
};
use pond_core::security::ports::policy::Principal;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

fn refuse(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

fn failed(what: &'static str, error: anyhow::Error) -> Response {
    tracing::warn!(%error, what, "a pairing lock request failed");
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        "pairing_lock_unavailable",
    )
}

/// Seats `profile_id` as the main member, which makes them the household's one owner. With
/// `replace` false it only fills an empty seat; `Ok(false)` when somebody already holds it.
pub(crate) async fn seat_main_member(
    state: &AppState,
    profile_id: &str,
    replace: bool,
) -> anyhow::Result<bool> {
    if !state
        .handshake
        .set_pairing_owner(profile_id, replace)
        .await?
    {
        return Ok(false);
    }
    state.profile_repo.hand_ownership_to(profile_id).await?;
    tracing::info!(
        kind = "household_owner_seated",
        profile_id,
        "the main member is the household's owner"
    );
    Ok(true)
}

/// `stored` as permission checks read it: the main member is the owner, and an `Owner` role
/// anyone else still carries counts as `Member`. An empty or unreadable seat leaves it as stored.
pub(crate) async fn seated_role(
    state: &AppState,
    profile_id: &str,
    stored: pond_core::user_data::domain::profile::Role,
) -> pond_core::user_data::domain::profile::Role {
    use pond_core::user_data::domain::profile::Role;
    let seat = match state.handshake.pairing_lock().await {
        Ok(lock) => lock.owner_profile_id,
        Err(error) => {
            tracing::warn!(%error, "could not read the main member; reading the stored role");
            None
        }
    };
    match seat {
        Some(main) if main == profile_id => Role::Owner,
        Some(_) if stored == Role::Owner => Role::Member,
        _ => stored,
    }
}

/// Tells the household of each phone that starts waiting, however it asked: over the home
/// network or over Bluetooth, which has no route to announce from.
pub fn spawn_request_watch(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut told: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Ok(waiting) = state.handshake.pairing_requests().await else {
                continue;
            };
            for request in &waiting {
                if told.insert(request.id.clone()) {
                    announce_request(&state, &request.device_name, &request.id).await;
                }
            }
            told.retain(|id| waiting.iter().any(|request| &request.id == id));
        }
    });
}

/// The household's notice that a phone is waiting. An alert: it must not wait for quiet hours
/// to end, since the request is gone in ten minutes.
async fn announce_request(state: &AppState, device_name: &str, request_id: &str) {
    tracing::info!(
        target: "giap::trace",
        kind = "pairing_request_opened",
        request = %request_id,
        "a phone asked to be paired"
    );
    let Some(sender) = state.notification_sender.clone() else {
        return;
    };
    let mut data = serde_json::Map::new();
    data.insert("action".into(), json!("pairing_request"));
    data.insert("request_id".into(), json!(request_id));
    let notice = Notification {
        id: format!("pairing-request-{request_id}"),
        target: "broadcast".to_string(),
        category: "alert".to_string(),
        title: "A phone asks to pair".to_string(),
        body: format!(
            "{device_name} asked to join this Pond. Its main member can accept or decline in Devices."
        ),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: Some(OpenScreen::Devices.merged_into(data)),
    };
    if let Err(error) = sender.broadcast(notice).await {
        tracing::warn!(%error, "could not tell the household a phone asked to pair");
    }
}

#[derive(Deserialize)]
pub struct ClaimBody {
    request_id: String,
    claim_secret: String,
}

fn claim_response(claim: PairingClaim, state: &AppState) -> Response {
    match claim {
        PairingClaim::Waiting => {
            (StatusCode::ACCEPTED, Json(json!({"status": "waiting"}))).into_response()
        }
        PairingClaim::Declined => {
            (StatusCode::FORBIDDEN, Json(json!({"status": "declined"}))).into_response()
        }
        PairingClaim::Gone => (StatusCode::GONE, Json(json!({"status": "gone"}))).into_response(),
        PairingClaim::Paired(tokens) => {
            let _ = state;
            Json(json!({
                "status": "paired",
                "accepted": true,
                "session_token": tokens.session_token,
                "refresh_token": tokens.refresh_token,
                "expires_at": tokens.expires_at,
            }))
            .into_response()
        }
    }
}

/// `POST /handshake/pairing-request/claim`: the asking phone comes back. `202` while it waits,
/// `403` declined, `410` gone, and the tokens once somebody let it in.
pub async fn claim(
    State(state): State<Arc<AppState>>,
    peer: Result<ConnectInfo<std::net::SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    Json(body): Json<ClaimBody>,
) -> Response {
    if let Err(refusal) = crate::network::require_lan(peer.ok()) {
        return refusal.into_response();
    }
    match state
        .handshake
        .claim_pairing_request(&body.request_id, &body.claim_secret)
        .await
    {
        Ok(claim) => {
            if let PairingClaim::Paired(tokens) = &claim {
                crate::routes::announce_paired_by_request(&state, &tokens.device_name).await;
            }
            claim_response(claim, &state)
        }
        Err(error) => failed("claim", error),
    }
}

#[derive(Deserialize)]
pub struct RecoverBody {
    request_id: String,
    claim_secret: String,
    pin: String,
}

/// `POST /handshake/pairing-request/recover`: the main member's new phone lets itself in with
/// the recovery PIN. A wrong PIN says how many tries are left; too many say how long to wait.
pub async fn recover(
    State(state): State<Arc<AppState>>,
    peer: Result<ConnectInfo<std::net::SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    Json(body): Json<RecoverBody>,
) -> Response {
    if let Err(refusal) = crate::network::require_lan(peer.ok()) {
        return refusal.into_response();
    }
    let checked = match state
        .handshake
        .recover_pairing_request(&body.request_id, &body.claim_secret, &body.pin)
        .await
    {
        Ok(checked) => checked,
        Err(error) => return failed("recover", error),
    };
    match checked {
        PinCheck::Accepted => {
            tracing::warn!(
                target: "giap::trace",
                kind = "pairing_recovered_with_pin",
                request = %body.request_id,
                "a phone let itself in with the recovery PIN"
            );
            claim(
                State(state),
                Ok(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))),
                Json(ClaimBody {
                    request_id: body.request_id,
                    claim_secret: body.claim_secret,
                }),
            )
            .await
        }
        PinCheck::Wrong { tries_left } => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "wrong_pin", "tries_left": tries_left})),
        )
            .into_response(),
        PinCheck::LockedFor(wait) => {
            let secs = wait.num_seconds().max(1);
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, secs.to_string())],
                Json(json!({"error": "pin_cooling", "retry_after_secs": secs})),
            )
                .into_response()
        }
        PinCheck::NoPin => refuse(StatusCode::CONFLICT, "no_recovery_pin"),
    }
}

/// `GET /pairing/lock`: the lock as any paired member may see it, and whether the caller may
/// change it.
pub async fn lock_state(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let lock = match state.handshake.pairing_lock().await {
        Ok(lock) => lock,
        Err(error) => return failed("state", error),
    };
    let can_change = crate::pond_admin::may_answer_pairing(
        &state,
        principal.as_ref(),
        lock.owner_profile_id.as_deref(),
    )
    .await
    .is_ok();
    Json(json!({
        "locked": lock.locked,
        "enforced": lock.enforced(),
        "owner_profile_id": lock.owner_profile_id,
        "has_recovery_pin": lock.has_recovery_pin,
        "owner_has_a_phone": lock.owner_has_a_phone,
        "min_pin_digits": MIN_PIN_DIGITS,
        "can_change": can_change,
    }))
    .into_response()
}

/// The device of a caller who may change the lock, or the refusal.
async fn changer(
    state: &AppState,
    principal: Option<&Extension<Principal>>,
) -> Result<String, Response> {
    let lock = state
        .handshake
        .pairing_lock()
        .await
        .map_err(|error| failed("state", error))?;
    crate::pond_admin::may_answer_pairing(state, principal, lock.owner_profile_id.as_deref()).await
}

#[derive(Deserialize)]
pub struct LockBody {
    locked: bool,
}

/// `PUT /pairing/lock` `{"locked": bool}`
pub async fn set_lock(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<LockBody>,
) -> Response {
    let device = match changer(&state, principal.as_ref()).await {
        Ok(device) => device,
        Err(refusal) => return refusal,
    };
    if let Err(error) = state.handshake.set_pairing_locked(body.locked).await {
        return failed("set", error);
    }
    tracing::info!(
        target: "giap::trace",
        kind = "pairing_lock_changed",
        locked = body.locked,
        device = %device,
        "the pairing lock was changed"
    );
    Json(json!({"locked": body.locked})).into_response()
}

#[derive(Deserialize)]
pub struct PinBody {
    pin: String,
}

/// `PUT /pairing/recovery-pin` `{"pin": "six to twelve digits"}`
pub async fn set_pin(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<PinBody>,
) -> Response {
    if let Err(refusal) = changer(&state, principal.as_ref()).await {
        return refusal;
    }
    if !pin_is_acceptable(&body.pin) {
        return refuse(StatusCode::BAD_REQUEST, "pin_not_six_to_twelve_digits");
    }
    match state.handshake.set_recovery_pin(Some(&body.pin)).await {
        Ok(()) => Json(json!({"has_recovery_pin": true})).into_response(),
        Err(error) => failed("pin", error),
    }
}

/// `DELETE /pairing/recovery-pin`
pub async fn clear_pin(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    if let Err(refusal) = changer(&state, principal.as_ref()).await {
        return refusal;
    }
    match state.handshake.set_recovery_pin(None).await {
        Ok(()) => Json(json!({"has_recovery_pin": false})).into_response(),
        Err(error) => failed("pin", error),
    }
}

#[derive(Deserialize)]
pub struct RecoverPinWithCardBody {
    admin_card: String,
    pin: String,
}

/// `POST /pairing/recovery-pin/recover`: reset a forgotten passcode using the giap.sh admin card.
pub async fn recover_pin_with_admin_card(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<RecoverPinWithCardBody>,
) -> Response {
    if !pin_is_acceptable(&body.pin) {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "pin_invalid");
    }
    let Some(admin) = state.pond_admin.as_ref() else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "admin_unavailable");
    };
    let device_id = principal
        .as_ref()
        .and_then(|Extension(p)| p.device_id.clone())
        .unwrap_or_else(|| "host".to_string());
    if let Err(err) = admin
        .elevation
        .unlock(&device_id, &body.admin_card, std::time::Instant::now())
        .await
    {
        return match err {
            pond_core::security::services::admin_elevation::UnlockRefusal::Cooling(wait) => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "cooling", "retry_after": wait.as_secs_ceil()})),
            )
                .into_response(),
            _ => refuse(StatusCode::FORBIDDEN, "admin_card_invalid"),
        };
    }
    match state.handshake.set_recovery_pin(Some(&body.pin)).await {
        Ok(()) => Json(json!({"has_recovery_pin": true})).into_response(),
        Err(error) => failed("recover_pin_with_admin_card", error),
    }
}

#[derive(Deserialize)]
pub struct OwnerBody {
    profile_id: String,
}

/// `PUT /pairing/owner` `{"profile_id": ...}`: hand the main member's role, and with it the
/// household's owner role, to another member.
pub async fn set_owner(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<OwnerBody>,
) -> Response {
    if let Err(refusal) = changer(&state, principal.as_ref()).await {
        return refusal;
    }
    let is_member = state
        .profile_repo
        .list()
        .await
        .map(|members| members.iter().any(|m| m.id == body.profile_id))
        .unwrap_or(false);
    if !is_member {
        return refuse(StatusCode::BAD_REQUEST, "not_a_member");
    }
    match seat_main_member(&state, &body.profile_id, true).await {
        Ok(_) => Json(json!({"owner_profile_id": body.profile_id})).into_response(),
        Err(error) => failed("owner", error),
    }
}

/// `GET /pairing/requests`: the phones waiting, for whoever may answer them.
pub async fn requests(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    if let Err(refusal) = changer(&state, principal.as_ref()).await {
        return refusal;
    }
    match state.handshake.pairing_requests().await {
        Ok(waiting) => Json(json!({ "requests": waiting })).into_response(),
        Err(error) => failed("requests", error),
    }
}

#[derive(Deserialize, Default)]
pub struct DecideBody {
    /// The member an accepted phone becomes; left out, the one its code named.
    #[serde(default)]
    profile_id: Option<String>,
}

async fn decide(
    state: Arc<AppState>,
    principal: Option<Extension<Principal>>,
    request_id: String,
    accept: bool,
    profile_id: Option<String>,
) -> Response {
    let device = match changer(&state, principal.as_ref()).await {
        Ok(device) => device,
        Err(refusal) => return refusal,
    };
    match state
        .handshake
        .decide_pairing_request(&request_id, accept, Some(&device), profile_id.as_deref())
        .await
    {
        Ok(true) => {
            tracing::info!(
                target: "giap::trace",
                kind = "pairing_request_answered",
                request = %request_id,
                accepted = accept,
                device = %device,
                "a pairing request was answered"
            );
            Json(json!({"id": request_id, "accepted": accept})).into_response()
        }
        Ok(false) => refuse(StatusCode::NOT_FOUND, "no_such_request"),
        Err(error) => failed("decide", error),
    }
}

/// `POST /pairing/requests/{id}/approve`
pub async fn approve(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let body: DecideBody = serde_json::from_slice(&body).unwrap_or_default();
    decide(state, principal, id, true, body.profile_id).await
}

/// `POST /pairing/requests/{id}/decline`
pub async fn decline(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Response {
    decide(state, principal, id, false, None).await
}
