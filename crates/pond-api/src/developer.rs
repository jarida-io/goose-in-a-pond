//! Developer mode (`/api/v1/developer/debug`): on a Jarida test Pond, a switch that lets the logs
//! show what the household said and the members' names. A home Pond says it is not a test Pond
//! and refuses the switch, whatever a phone sends.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use pond_core::security::domain::developer_mode::DebugSwitch;
use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use pond_core::security::ports::policy::Principal;
use pond_core::security::ports::test_pond::TestPondMark;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

/// The host's test Pond mark and the debug switch the log scrubber reads. The default has no
/// mark, so it is a home Pond.
#[derive(Default)]
pub struct DeveloperMode {
    mark: Option<Arc<dyn TestPondMark>>,
    debug: DebugSwitch,
}

impl DeveloperMode {
    pub fn new(mark: Arc<dyn TestPondMark>, debug: DebugSwitch) -> Self {
        Self {
            mark: Some(mark),
            debug,
        }
    }

    /// `(test_pond, debug)`. Taking the mark away closes the switch at the next look, which the
    /// server also takes every minute so an unwatched Pond stops showing content too.
    pub async fn current(&self) -> (bool, bool) {
        let test_pond = match &self.mark {
            Some(mark) => mark.is_test_pond().await.unwrap_or_else(|e| {
                tracing::warn!(error = %e, "developer mode: could not read the test Pond mark");
                false
            }),
            None => false,
        };
        if !test_pond && self.debug.is_on() {
            self.debug.set(false);
            tracing::warn!("developer mode: debug is off, because this is no longer a test Pond");
        }
        (test_pond, self.debug.is_on())
    }
}

/// `GET /api/v1/developer/debug`: `{test_pond, debug}`.
pub async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let (test_pond, debug) = state.developer.current().await;
    Json(json!({ "test_pond": test_pond, "debug": debug }))
}

#[derive(Deserialize)]
pub struct DebugBody {
    enabled: bool,
}

/// `PUT /api/v1/developer/debug` with `{enabled}`; `409 not_a_test_pond` on a home Pond.
pub async fn set_debug(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<DebugBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (test_pond, was) = state.developer.current().await;
    if !test_pond {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "not_a_test_pond" })),
        ));
    }
    state.developer.debug.set(body.enabled);
    if was != body.enabled {
        let device = principal
            .as_ref()
            .and_then(|Extension(p)| p.device_id.clone())
            .unwrap_or_default();
        // A WARN reaches the Logs viewer, so whoever reads content there can see when it began.
        tracing::warn!(
            device_id = %device,
            "developer mode: debug {}",
            if body.enabled { "on, so the logs show household content" } else { "off" }
        );
        if let Some(log) = state.event_log.as_ref() {
            let event = Event::new(EventCategory::Auth, "developer.debug_switched")
                .sensitivity(PrivacySensitivity::Sensitive)
                .attr("device_id", device)
                .attr("enabled", body.enabled);
            if let Err(e) = log.append(event).await {
                tracing::warn!(error = %e, "failed to record the debug switch");
            }
        }
    }
    Ok(Json(json!({ "test_pond": true, "debug": body.enabled })))
}
