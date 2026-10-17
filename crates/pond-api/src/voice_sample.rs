//! `POST /api/v1/voice/sample`: the pond reads one fixed sentence, in a voice the household is
//! choosing, on its own speaker, and changes no setting (`docs/architecture/pond-networking.md`).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pond_core::models::services::voice::voice_sample::{VoiceSampleLimiter, VOICE_SAMPLE_SENTENCE};
use pond_core::network::domain::claim::RetryAfter;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

/// A voice id is a few dozen bytes; a body anywhere near this is not a sample request.
pub(crate) const VOICE_SAMPLE_BODY_LIMIT: usize = 1024;

/// One limiter for every caller, so two phones cannot double the rate.
#[derive(Default)]
pub struct VoiceSamples {
    limiter: Mutex<VoiceSampleLimiter>,
}

impl VoiceSamples {
    /// Takes the slot, or the whole seconds until it frees. A poisoned lock is recovered: every
    /// write leaves the limiter consistent.
    fn try_acquire(&self, now: Instant) -> Result<(), u64> {
        self.limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_acquire(now)
            .map_err(|wait| RetryAfter(wait).as_secs_ceil().max(1))
    }
}

/// The voice only: the sentence is fixed, so no caller's text reaches the speaker.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VoiceSampleRequest {
    voice: String,
}

fn refuse(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// Order matters: the body, the engine, the voice, and only then the limiter, so a typo or a
/// pond with no voice does not cost the household its next sample.
pub(crate) async fn voice_sample(
    State(state): State<Arc<AppState>>,
    body: Result<Json<VoiceSampleRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return refuse(StatusCode::BAD_REQUEST, "invalid request body");
    };
    let (Some(control), Some(speaker)) = (state.tts_control.clone(), state.tts.clone()) else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "voice_unavailable");
    };
    if !control.installed_voices().await.contains(&request.voice) {
        return refuse(StatusCode::NOT_FOUND, "unknown_voice");
    }
    if let Err(retry_after) = state.voice_samples.try_acquire(Instant::now()) {
        return crate::routes::too_many_requests("voice_sample_limited", retry_after);
    }

    tracing::info!(
        kind = "voice_sample",
        voice = %request.voice,
        "reading the voice sample aloud"
    );
    // Detached like the spoken code: a first utterance loads the voice engine. Played without
    // `begin_utterance`, which would cut off a turn that is speaking; a turn that starts wins.
    let voice = request.voice;
    tokio::spawn(async move {
        let played = async {
            let wav = control
                .synthesize_in_voice(&voice, VOICE_SAMPLE_SENTENCE)
                .await?;
            speaker.play_audio(wav).await
        }
        .await;
        if let Err(error) = played {
            tracing::warn!(
                kind = "voice_sample_failed",
                voice = %voice,
                error = %format!("{error:#}"),
                "the pond could not read the voice sample aloud"
            );
        }
    });

    (StatusCode::ACCEPTED, Json(json!({ "speaking": true }))).into_response()
}
