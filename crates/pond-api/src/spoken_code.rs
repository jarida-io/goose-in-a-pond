//! `POST /api/v1/handshake/spoken-code`: the pond reads a fresh pairing code aloud. No answer
//! ever carries the code, so only someone in earshot can use it (`docs/architecture/pond-networking.md`).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::rejection::ExtensionRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pond_core::network::ports::code_announcer::CodeAnnouncer;
use pond_core::network::services::spoken_code_limiter::SpokenCodeLimiter;
use serde_json::json;

use crate::AppState;

/// What the route needs beyond the handshake: one limiter for every caller, and the speaker.
pub struct SpokenCodes {
    limiter: Mutex<SpokenCodeLimiter>,
    announcer: Option<Arc<dyn CodeAnnouncer>>,
}

impl Default for SpokenCodes {
    /// No announcer: every request is `503 announcer_unavailable`.
    fn default() -> Self {
        Self::new(None)
    }
}

impl SpokenCodes {
    pub fn new(announcer: Option<Arc<dyn CodeAnnouncer>>) -> Self {
        Self {
            limiter: Mutex::new(SpokenCodeLimiter::new()),
            announcer,
        }
    }

    /// Takes a slot, or the seconds until one frees. A poisoned lock is recovered: every write
    /// leaves the limiter consistent.
    fn try_acquire(&self, now: Instant) -> Result<(), u64> {
        self.limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_acquire(now)
            .map_err(|retry_after| retry_after.as_secs_ceil())
    }
}

fn refuse(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// Order matters: the household's switch, then the limiter (which charges), then the speaker,
/// and only then is a code issued, since issuing retires the code on any screen.
pub(crate) async fn handshake_spoken_code(
    State(state): State<Arc<AppState>>,
    peer: Result<ConnectInfo<std::net::SocketAddr>, ExtensionRejection>,
) -> Response {
    let peer = peer.ok();
    if let Err(refusal) = crate::network::require_lan(peer) {
        return refusal.into_response();
    }
    let peer_ip = peer.map(|ConnectInfo(p)| p.ip());

    match state.settings_repo.get().await {
        Ok(settings) if settings.spoken_pairing_codes => {}
        Ok(_) => return refuse(StatusCode::FORBIDDEN, "spoken_codes_disabled"),
        // Fail closed: a household that turned this off must not hear a code on a bad read.
        Err(error) => {
            return crate::routes::handshake_error("spoken_code_settings", error).into_response()
        }
    }

    if let Err(retry_after) = state.spoken_codes.try_acquire(Instant::now()) {
        return crate::routes::too_many_requests("spoken_code_limited", retry_after);
    }

    let announcer = match state.spoken_codes.announcer.clone() {
        Some(announcer) if announcer.is_available().await => announcer,
        _ => {
            tracing::warn!(
                kind = "spoken_code_unavailable",
                "a phone asked for a spoken pairing code, but this pond cannot make a sound"
            );
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "announcer_unavailable");
        }
    };

    // The same default attribution as a code issued on the host: a household of one owns it.
    let member_ids: Vec<String> = match state.profile_repo.list().await {
        Ok(profiles) => profiles.into_iter().map(|p| p.id).collect(),
        Err(error) => {
            tracing::warn!(
                %error,
                "could not list household members; issuing an unattributed spoken code"
            );
            Vec::new()
        }
    };
    let owner = pond_core::user_data::services::member_attribution::owner_for_new_code(
        None,
        false,
        &member_ids,
    );
    let issued = match state
        .handshake
        .issue_pairing_code_for(owner.as_deref())
        .await
    {
        Ok(issued) => issued,
        Err(error) => {
            return crate::routes::handshake_error("spoken_code_issue", error).into_response()
        }
    };

    // Whole seconds, so no run of digits in this answer can pass for a code.
    let expires_at = chrono::DateTime::parse_from_rfc3339(&issued.expires_at)
        .map(|at| {
            at.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| issued.expires_at.clone());
    tracing::info!(
        kind = "spoken_code_issued",
        peer = ?peer_ip,
        attributed = owner.is_some(),
        expires_at = %expires_at,
        "reading a pairing code aloud"
    );
    record_spoken_code(&state, owner.is_some()).await;

    // Detached: a first utterance loads the voice, and a phone should not wait seconds to learn
    // that the pond has started speaking.
    let digits = issued.code;
    tokio::spawn(async move {
        if let Err(error) = announcer.announce_code(&digits).await {
            tracing::warn!(
                kind = "spoken_code_failed",
                %error,
                "the pond could not read the pairing code aloud; ask for another in 30 s"
            );
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "spoken": true, "expires_at": expires_at })),
    )
        .into_response()
}

/// The audit trail says a code was read aloud, never what it was.
async fn record_spoken_code(state: &AppState, attributed: bool) {
    use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};

    let Some(event_log) = state.event_log.as_ref() else {
        return;
    };
    let event = Event::new(EventCategory::Auth, "auth.spoken_code_issued")
        .sensitivity(PrivacySensitivity::Sensitive)
        .attr("attributed", attributed);
    if let Err(error) = event_log.append(event).await {
        tracing::warn!(%error, "failed to record the spoken pairing code event");
    }
}
