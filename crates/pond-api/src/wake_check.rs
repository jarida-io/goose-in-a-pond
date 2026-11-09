//! `POST /api/v1/voice/wake/check`: whether a short stretch of speech holds the household's wake
//! phrase, for a phone that listens on the household's behalf.
//!
//! A phone has its own detector for "hey goose" only. For any other phrase it cuts the speech it
//! hears into segments, as the ear does for Whisper, and asks here: the transcript is matched with
//! the room's own triggers (the phrase plus the pronunciations learned for it,
//! `pond_voice::text::wake_triggers`), so a phrase or a pronunciation changed on the Pond counts
//! from the next segment, on every phone. The answer also names the phrase, so a phone learns when
//! the household has gone back to "hey goose".
//!
//! Nothing is kept. The recording is transcribed and dropped, and the transcript is never logged
//! or returned: a phone sends whatever was said near it, and only "was it the phrase" leaves here.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;

/// Longest segment a phone sends: the ear's own segment cap (8 s), with room for the WAV header,
/// the speech pad and the multipart envelope. At 16 kHz mono 16-bit, 8 s is 256,000 bytes.
pub const WAKE_CHECK_BODY_LIMIT: usize = 320 * 1024;

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, error: &str) -> Refusal {
    (status, Json(json!({"error": error})))
}

fn failed(e: impl std::fmt::Display) -> Refusal {
    tracing::warn!(kind = "wake_check", error = %e, "wake check failed");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, "wake_check_failed")
}

/// A multipart failure with axum's own status: 413 for a segment past [`WAKE_CHECK_BODY_LIMIT`].
fn unreadable(e: axum::extract::multipart::MultipartError) -> Refusal {
    let status = e.status();
    refuse(
        status,
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            "segment_too_long"
        } else {
            "multipart_error"
        },
    )
}

async fn read_audio(mut multipart: Multipart) -> Result<Vec<u8>, Refusal> {
    while let Some(field) = multipart.next_field().await.map_err(unreadable)? {
        if field.name() == Some("audio") {
            return field.bytes().await.map(|b| b.to_vec()).map_err(unreadable);
        }
    }
    Err(refuse(StatusCode::BAD_REQUEST, "missing_audio"))
}

/// `POST /api/v1/voice/wake/check`
///
/// **Request**: multipart/form-data with an `audio` field, a WAV of at most 8 s.
///
/// **Response**: `{"heard": true, "phrase": "hey jarvis"}`. `heard` is false for speech that is
/// not the phrase and for a segment with no words. 503 `speech_recognition_unavailable` on a Pond
/// built without in-process speech recognition.
pub(crate) async fn check_wake(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, Refusal> {
    let started = Instant::now();
    let audio = read_audio(multipart).await?;
    pond_voice::dsp::decode_wav(&audio)
        .map_err(|_| refuse(StatusCode::BAD_REQUEST, "audio_must_be_wav"))?;
    let Some(transcribe) = state.transcribe_audio.clone() else {
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_recognition_unavailable",
        ));
    };
    let bytes = audio.len();
    let transcript = tokio::task::spawn_blocking(move || transcribe(audio))
        .await
        .map_err(failed)?
        .map_err(failed)?;

    let settings = state.settings_repo.get().await.map_err(failed)?;
    let triggers = pond_voice::text::wake_triggers(
        &settings.voice_wake_word,
        &settings.voice_wake_word_transcriptions,
    );
    let heard = pond_voice::text::heard_wake(&transcript, &triggers);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if heard {
        tracing::info!(
            kind = "wake_check",
            heard,
            bytes,
            elapsed_ms,
            "a phone heard the wake phrase"
        );
    } else {
        tracing::debug!(
            kind = "wake_check",
            heard,
            bytes,
            elapsed_ms,
            "a phone's segment was not the wake phrase"
        );
    }
    Ok(Json(json!({
        "heard": heard,
        "phrase": settings.voice_wake_word,
    })))
}
