//! `/api/v1/admin/*`: My Pond. A member's device unlocks it with the admin card or its own key;
//! every route but status, unlock, lock and forgetting the key then needs that unlock to be live.
//! The design and the wire contract are in `docs/architecture/pond-administrator.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use pond_core::security::domain::admin_card::ADMIN_PRINCIPAL;
use pond_core::security::domain::event::{
    AttributeValue, Event, EventCategory, PrivacySensitivity,
};
use pond_core::security::domain::proven_device::{DeviceRung, ProvenDevice};
use pond_core::security::ports::policy::Principal;
use pond_core::security::services::admin_elevation::{AdminElevation, Locked, UnlockRefusal};
use pond_core::shared::domain::pond_admin::{AdminError, DataPath};
use pond_core::shared::services::pond_admin::PondAdminService;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// My Pond's two halves: who is unlocked, and what an unlocked device may do.
pub struct PondAdmin {
    pub service: PondAdminService,
    pub elevation: AdminElevation,
    started: Instant,
}

impl PondAdmin {
    pub fn new(service: PondAdminService, elevation: AdminElevation) -> Self {
        Self {
            service,
            elevation,
            started: Instant::now(),
        }
    }
}

/// A member's device: the only caller My Pond answers.
struct Caller {
    device: String,
    member: String,
}

fn refuse(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

fn admin_of(state: &AppState) -> Result<Arc<PondAdmin>, Response> {
    state
        .pond_admin
        .clone()
        .ok_or_else(|| refuse(StatusCode::SERVICE_UNAVAILABLE, "admin_unavailable"))
}

/// The caller's device and its member. The device comes only from the token; an unattributed
/// device or an unreadable attribution is nobody's, so a guest's phone never reaches My Pond.
async fn member_device(
    state: &AppState,
    principal: Option<&Extension<Principal>>,
) -> Result<Caller, Response> {
    let device = match principal {
        Some(Extension(principal)) => ProvenDevice::from_principal(principal),
        None => ProvenDevice::none(),
    };
    let Some(device_id) = device.id().map(str::to_owned) else {
        return Err(refuse(StatusCode::FORBIDDEN, "admin_members_only"));
    };
    let attribution = pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
        state.db.system.clone(),
    )
    .device_profile(&device_id)
    .await;
    match device.rung(attribution) {
        DeviceRung::Member(member) => Ok(Caller {
            device: device_id,
            member,
        }),
        DeviceRung::Unavailable(why) => {
            tracing::warn!(device = %device_id, error = %why, "admin: could not read the device's member");
            Err(refuse(StatusCode::FORBIDDEN, "admin_members_only"))
        }
        DeviceRung::NoDevice | DeviceRung::Unattributed => {
            Err(refuse(StatusCode::FORBIDDEN, "admin_members_only"))
        }
    }
}

/// A member's device whose unlock is live; each admitted request renews it.
async fn unlocked(
    state: &AppState,
    principal: Option<&Extension<Principal>>,
) -> Result<(Arc<PondAdmin>, Caller), Response> {
    let admin = admin_of(state)?;
    let caller = member_device(state, principal).await?;
    if let Err(locked) = admin.elevation.admit(&caller.device, Instant::now()).await {
        tracing::info!(
            kind = "admin_locked",
            device = %caller.device,
            reason = locked.as_str(),
            "refused an admin request from a locked device"
        );
        return Err(refuse(StatusCode::FORBIDDEN, "admin_locked"));
    }
    Ok((admin, caller))
}

fn admin_error(error: AdminError) -> Response {
    let status = match &error {
        AdminError::InvalidPath | AdminError::UnknownPowerMode => StatusCode::BAD_REQUEST,
        AdminError::NotFound | AdminError::UnknownCache => StatusCode::NOT_FOUND,
        AdminError::NotADirectory | AdminError::TaskActive | AdminError::LongTaskRunning => {
            StatusCode::CONFLICT
        }
        AdminError::Protected(_) => StatusCode::FORBIDDEN,
        AdminError::PowerControlUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        AdminError::Internal(why) => {
            tracing::warn!(error = %why, "admin request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let mut body = json!({ "error": error.code() });
    if let AdminError::Protected(protection) = &error {
        body["reason"] = json!(protection.as_str());
    }
    (status, Json(body)).into_response()
}

/// Every unlock attempt and every change is on the audit trail, under the administrator's name
/// and the device that acted. Paths are kept to their top-level area: a file's name may be personal.
async fn audit(
    state: &AppState,
    action: &'static str,
    device: &str,
    attrs: Vec<(&str, AttributeValue)>,
) {
    let Some(log) = state.event_log.as_ref() else {
        return;
    };
    let mut event = Event::new(EventCategory::Auth, action)
        .sensitivity(PrivacySensitivity::Sensitive)
        .attr("principal", ADMIN_PRINCIPAL)
        .attr("device_id", device);
    for (key, value) in attrs {
        event = event.attr(key, value);
    }
    if let Err(e) = log.append(event).await {
        tracing::warn!(error = %e, action, "failed to record an admin event");
    }
}

fn bytes(n: u64) -> AttributeValue {
    i64::try_from(n).unwrap_or(i64::MAX).into()
}

fn area_of(path: &str) -> String {
    DataPath::parse(path)
        .ok()
        .and_then(|p| p.segments().next().map(str::to_owned))
        .unwrap_or_default()
}

fn expires_at(left: Duration) -> String {
    (Utc::now() + chrono::Duration::from_std(left).unwrap_or_default()).to_rfc3339()
}

/// Tell the household, as a new pairing does: an unlock they did not expect is worth a look.
async fn announce_unlock(state: &AppState, device: &str) {
    let Some(sender) = state.notification_sender.as_ref() else {
        return;
    };
    let name = match state.device_registry.get_device(device).await {
        Ok(Some(found)) => found.name,
        _ => "A device".to_string(),
    };
    use pond_core::mcp::ports::notification::{Notification, OpenScreen};
    let notice = Notification {
        id: uuid::Uuid::new_v4().to_string(),
        target: "broadcast".to_string(),
        category: "alert".to_string(),
        title: "My Pond unlocked".to_string(),
        body: format!(
            "\"{name}\" unlocked your Pond's administration. If nobody in your household did, \
             replace the admin card on the Pond."
        ),
        timestamp: Utc::now().to_rfc3339(),
        data: Some(OpenScreen::Devices.data()),
    };
    if let Err(e) = sender.broadcast(notice).await {
        tracing::warn!(error = %e, "failed to push the admin unlock notice");
    }
}

/// `GET /api/v1/admin/status`
pub async fn status(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let admin = match admin_of(&state) {
        Ok(admin) => admin,
        Err(refusal) => return refusal,
    };
    let caller = match member_device(&state, principal.as_ref()).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    match admin.elevation.status(&caller.device, Instant::now()).await {
        Ok(status) => Json(json!({
            "card": if status.card_present { "present" } else { "missing" },
            "key": status.key_held,
            "unlocked": status.unlocked_for.is_some(),
            "expires_at": status.unlocked_for.map(expires_at),
            "idle_minutes": admin.elevation.idle().as_secs() / 60,
            "retry_after": status.retry_after.map(|r| r.as_secs_ceil()),
        }))
        .into_response(),
        Err(why) => {
            tracing::warn!(error = %why, "admin: could not read the admin card");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "admin_card_unreadable")
        }
    }
}

/// The card's password, or the key the pond issued this device; a key wins if both are sent.
#[derive(Deserialize)]
pub struct UnlockBody {
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

/// `POST /api/v1/admin/unlock {password}` or `{key}`
pub async fn unlock(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<UnlockBody>,
) -> Response {
    let admin = match admin_of(&state) {
        Ok(admin) => admin,
        Err(refusal) => return refusal,
    };
    let caller = match member_device(&state, principal.as_ref()).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let now = Instant::now();
    let by_key = body.key.is_some();
    let attempt = match (body.key, body.password) {
        (Some(key), _) => {
            admin
                .elevation
                .unlock_with_key(&caller.device, &key, now)
                .await
        }
        (None, Some(password)) => admin.elevation.unlock(&caller.device, &password, now).await,
        (None, None) => Err(UnlockRefusal::Malformed),
    };
    match attempt {
        Ok(left) => {
            tracing::info!(kind = "admin_unlocked", device = %caller.device, by_key, "My Pond unlocked");
            audit(
                &state,
                "admin.unlocked",
                &caller.device,
                vec![("by_key", by_key.into())],
            )
            .await;
            announce_unlock(&state, &caller.device).await;
            Json(json!({ "unlocked": true, "expires_at": expires_at(left) })).into_response()
        }
        Err(refusal) => {
            tracing::warn!(
                kind = "admin_unlock_refused",
                device = %caller.device,
                reason = refusal.code(),
                "My Pond unlock refused"
            );
            if matches!(
                refusal,
                UnlockRefusal::Rejected | UnlockRefusal::KeyRejected
            ) {
                audit(&state, "admin.unlock_refused", &caller.device, vec![]).await;
            }
            let code = refusal.code();
            match refusal {
                UnlockRefusal::Malformed => refuse(StatusCode::BAD_REQUEST, code),
                UnlockRefusal::Rejected | UnlockRefusal::KeyRejected => {
                    refuse(StatusCode::FORBIDDEN, code)
                }
                UnlockRefusal::CardMissing => refuse(StatusCode::CONFLICT, code),
                UnlockRefusal::Cooling(retry) => {
                    let secs = retry.as_secs_ceil();
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(header::RETRY_AFTER, secs.to_string())],
                        Json(json!({ "error": code, "retry_after": secs })),
                    )
                        .into_response()
                }
                UnlockRefusal::Unavailable(why) => {
                    tracing::warn!(error = %why, "admin: could not read the admin card");
                    refuse(StatusCode::SERVICE_UNAVAILABLE, code)
                }
            }
        }
    }
}

/// `POST /api/v1/admin/lock`
pub async fn lock(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let admin = match admin_of(&state) {
        Ok(admin) => admin,
        Err(refusal) => return refusal,
    };
    match member_device(&state, principal.as_ref()).await {
        Ok(caller) => {
            admin.elevation.lock(&caller.device).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(refusal) => refusal,
    }
}

/// `POST /api/v1/admin/enroll`: an unlocked device's own key, shown once, replacing any older one.
pub async fn enroll(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let admin = match admin_of(&state) {
        Ok(admin) => admin,
        Err(refusal) => return refusal,
    };
    let caller = match member_device(&state, principal.as_ref()).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    match admin.elevation.enroll(&caller.device, Instant::now()).await {
        Ok(key) => {
            audit(&state, "admin.key_issued", &caller.device, vec![]).await;
            Json(json!({ "key": key })).into_response()
        }
        Err(Locked::Unavailable(why)) => {
            tracing::warn!(error = %why, "admin: could not issue a device key");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "admin_key_unavailable")
        }
        Err(_) => refuse(StatusCode::FORBIDDEN, "admin_locked"),
    }
}

/// `DELETE /api/v1/admin/key`: this device forgets My Pond; its key is revoked and it is locked.
pub async fn forget_key(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let admin = match admin_of(&state) {
        Ok(admin) => admin,
        Err(refusal) => return refusal,
    };
    let caller = match member_device(&state, principal.as_ref()).await {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    match admin.elevation.revoke_device(&caller.device).await {
        Ok(_) => {
            audit(&state, "admin.key_revoked", &caller.device, vec![]).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "admin: could not revoke a device key");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "admin_key_unavailable")
        }
    }
}

/// The household's first activation user: owner setup completing on the device that claimed the
/// pond, now a member's, earns that device its key without the card. The caller has checked that
/// this request is the one that completed onboarding.
pub async fn first_activation_key(
    state: &AppState,
    principal: Option<&Extension<Principal>>,
) -> Option<String> {
    let admin = state.pond_admin.clone()?;
    let claims = state.setup_claims.as_ref()?;
    let caller = member_device(state, principal).await.ok()?;
    let claimant = claims.claimed_by().await.ok().flatten()?;
    if claimant != caller.device {
        return None;
    }
    match admin.elevation.issue_first_key(&caller.device).await {
        Ok(key) => {
            tracing::info!(kind = "admin_first_key", device = %caller.device, "the first activation user's phone holds My Pond's key");
            audit(
                state,
                "admin.key_issued",
                &caller.device,
                vec![("first_activation", true.into())],
            )
            .await;
            Some(key)
        }
        Err(e) => {
            tracing::warn!(error = %e, "admin: could not issue the first activation user's key");
            None
        }
    }
}

macro_rules! unlocked_or_refuse {
    ($state:expr, $principal:expr) => {
        match unlocked(&$state, $principal.as_ref()).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        }
    };
}

/// `GET /api/v1/admin/overview`
pub async fn overview(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let (admin, _) = unlocked_or_refuse!(state, principal);
    match admin.service.overview().await {
        Ok(overview) => Json(overview).into_response(),
        Err(e) => admin_error(e),
    }
}

#[derive(Deserialize)]
pub struct FilesQuery {
    #[serde(default)]
    path: String,
}

/// `GET /api/v1/admin/files?path=`
pub async fn list_files(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(query): Query<FilesQuery>,
) -> Response {
    let (admin, _) = unlocked_or_refuse!(state, principal);
    match admin.service.browse(&query.path).await {
        Ok(listing) => Json(listing).into_response(),
        Err(e) => admin_error(e),
    }
}

/// `DELETE /api/v1/admin/files?path=`
pub async fn delete_file(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(query): Query<FilesQuery>,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    match admin.service.delete(&query.path).await {
        Ok(freed) => {
            audit(
                &state,
                "admin.file_deleted",
                &caller.device,
                vec![
                    ("area", area_of(&query.path).into()),
                    ("freed_bytes", bytes(freed)),
                ],
            )
            .await;
            Json(json!({ "freed_bytes": freed })).into_response()
        }
        Err(e) => admin_error(e),
    }
}

/// `GET /api/v1/admin/caches`
pub async fn caches(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let (admin, _) = unlocked_or_refuse!(state, principal);
    match admin.service.caches().await {
        Ok(caches) => Json(json!({ "caches": caches })).into_response(),
        Err(e) => admin_error(e),
    }
}

/// `POST /api/v1/admin/caches/{id}/clear`
pub async fn clear_cache(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    match admin.service.clear_cache(&id).await {
        Ok(freed) => {
            audit(
                &state,
                "admin.cache_cleared",
                &caller.device,
                vec![("cache", id.clone().into()), ("freed_bytes", bytes(freed))],
            )
            .await;
            Json(json!({ "freed_bytes": freed })).into_response()
        }
        Err(e) => admin_error(e),
    }
}

/// `GET /api/v1/admin/tasks`
pub async fn tasks(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    match admin.service.tasks(&caller.member).await {
        Ok(tasks) => Json(json!({ "tasks": tasks })).into_response(),
        Err(e) => admin_error(e),
    }
}

/// `DELETE /api/v1/admin/tasks/{id}`
pub async fn delete_task(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    match admin.service.delete_task(&id).await {
        Ok(()) => {
            audit(
                &state,
                "admin.task_deleted",
                &caller.device,
                vec![("task_id", id.clone().into())],
            )
            .await;
            Json(json!({ "deleted": true })).into_response()
        }
        Err(e) => admin_error(e),
    }
}

/// `DELETE /api/v1/admin/tasks/{id}/artifacts/{artifact_id}`
pub async fn delete_artifact(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((id, artifact_id)): Path<(String, String)>,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    match admin.service.delete_artifact(&id, &artifact_id).await {
        Ok(()) => {
            audit(
                &state,
                "admin.artifact_deleted",
                &caller.device,
                vec![
                    ("task_id", id.clone().into()),
                    ("artifact_id", artifact_id.clone().into()),
                ],
            )
            .await;
            Json(json!({ "deleted": true })).into_response()
        }
        Err(e) => admin_error(e),
    }
}

/// `GET /api/v1/admin/health`
pub async fn health(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let (admin, _) = unlocked_or_refuse!(state, principal);
    match admin.service.health().await {
        Ok(report) => {
            let mut body = serde_json::to_value(&report).unwrap_or_else(|_| json!({}));
            body["version"] = json!(env!("CARGO_PKG_VERSION"));
            body["uptime_secs"] = json!(admin.started.elapsed().as_secs());
            Json(body).into_response()
        }
        Err(e) => admin_error(e),
    }
}

/// `GET /api/v1/admin/power`
pub async fn power(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let (admin, _) = unlocked_or_refuse!(state, principal);
    match admin.service.power().await {
        Ok(power) => Json(power).into_response(),
        Err(e) => admin_error(e),
    }
}

#[derive(Deserialize)]
pub struct ModeBody {
    id: u32,
}

/// `PUT /api/v1/admin/power/mode {id}`
pub async fn set_power_mode(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<ModeBody>,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    match admin.service.set_power_mode(body.id).await {
        Ok(set) => {
            audit(
                &state,
                "admin.power_mode_set",
                &caller.device,
                vec![
                    ("mode", set.mode.name.clone().into()),
                    ("reboot_required", set.reboot_required.into()),
                ],
            )
            .await;
            Json(set).into_response()
        }
        Err(e) => admin_error(e),
    }
}

#[derive(Deserialize, Default)]
struct RestartBody {
    #[serde(default)]
    force: bool,
}

/// `POST /api/v1/admin/power/restart {force?}`. The body is optional, so it is read by hand.
pub async fn restart(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Bytes,
) -> Response {
    let (admin, caller) = unlocked_or_refuse!(state, principal);
    let body: RestartBody = if body.is_empty() {
        RestartBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(body) => body,
            Err(_) => return refuse(StatusCode::BAD_REQUEST, "invalid_body"),
        }
    };
    // Recorded before the restart: afterwards there may be no process left to record it.
    audit(
        &state,
        "admin.restart_requested",
        &caller.device,
        vec![("force", body.force.into())],
    )
    .await;
    match admin.service.restart(body.force).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({ "restarting": true }))).into_response(),
        Err(e) => admin_error(e),
    }
}
