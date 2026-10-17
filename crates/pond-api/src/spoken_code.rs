//! `POST /api/v1/handshake/spoken-code`: the pond reads a fresh pairing code aloud. The decision
//! is [`SpokenCodeService`]'s, shared with the setup channel, so both transports draw on one limit
//! and neither answer ever carries the code (`docs/architecture/pond-networking.md`).

use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::ExtensionRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pond_core::network::services::spoken_code::{
    SpokenCodeDeps, SpokenCodeRefusal, SpokenCodeService, SpokenCodeVia,
};
use serde_json::json;

use crate::AppState;

/// The route's limiter and speaker: the pond's one [`SpokenCodeService`].
pub type SpokenCodes = SpokenCodeService;

fn refuse(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

pub(crate) async fn handshake_spoken_code(
    State(state): State<Arc<AppState>>,
    peer: Result<ConnectInfo<std::net::SocketAddr>, ExtensionRejection>,
) -> Response {
    let peer = peer.ok();
    if let Err(refusal) = crate::network::require_lan(peer) {
        return refusal.into_response();
    }
    let deps = SpokenCodeDeps {
        settings: state.settings_repo.as_ref(),
        profiles: state.profile_repo.as_ref(),
        handshake: state.handshake.as_ref(),
        event_log: state.event_log.as_deref(),
    };
    let via = SpokenCodeVia::Lan {
        peer: peer.map(|ConnectInfo(p)| p.ip()),
    };
    match state.spoken_codes.request(deps, via, Instant::now()).await {
        Ok(Ok(issued)) => (
            StatusCode::ACCEPTED,
            Json(json!({ "spoken": true, "expires_at": issued.expires_at })),
        )
            .into_response(),
        Ok(Err(refusal @ SpokenCodeRefusal::Disabled)) => {
            refuse(StatusCode::FORBIDDEN, refusal.code())
        }
        Ok(Err(refusal @ SpokenCodeRefusal::Limited { retry_after })) => {
            crate::routes::too_many_requests(refusal.code(), retry_after.as_secs_ceil())
        }
        Ok(Err(refusal @ SpokenCodeRefusal::AnnouncerUnavailable)) => {
            refuse(StatusCode::SERVICE_UNAVAILABLE, refusal.code())
        }
        Err(error) => {
            crate::routes::handshake_error("spoken_code", format_args!("{error:#}")).into_response()
        }
    }
}
