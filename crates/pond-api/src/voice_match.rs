//! Voice Match and wake calibration in one enrolment flow: `POST /voice/takes` (five sentences,
//! each starting with the wake phrase), `GET /voice/prints`, forgetting a voice, and the model.
//! Recordings are never kept: a take becomes a CNN verdict and, with Voice Match on, a print.

use std::sync::Arc;

use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pond_core::models::domain::voice_match_model;
use pond_core::models::domain::wake_detector::{household_wake_detector, WakeDetector};
use pond_core::models::ports::voice_match::{
    TakeError, VoiceMatchEngine, VoiceModelState, VOICE_MATCH_CREDIT,
};
use pond_core::user_data::domain::voice_print::{VoicePrint, WakeTake};
use pond_core::user_data::ports::voice_print::VoicePrintRepository;
use pond_core::user_data::services::voice_match::{
    latest_runs, members, replaceable, wake_verdict, TAKES_PER_RUN,
};
use serde_json::{json, Value};

use crate::AppState;

type Refusal = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, body: Value) -> Refusal {
    (status, Json(body))
}

fn failed(e: impl std::fmt::Display) -> Refusal {
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"error": e.to_string()}),
    )
}

/// Same stand-in as `proposal_repo`: the prints are plain tables beside the profiles.
pub(crate) fn voice_prints(
    state: &AppState,
) -> pond_infra::sqlite_voice_recognition::SqliteVoiceRecognition {
    pond_infra::sqlite_voice_recognition::SqliteVoiceRecognition::new(state.db.system.clone())
}

fn engine(state: &AppState) -> Result<&Arc<dyn VoiceMatchEngine>, Refusal> {
    state.voice_match.as_ref().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "voice_match_unavailable"}),
        )
    })
}

fn household_changed(state: &AppState) {
    if let Some(room) = &state.room_voice {
        room.household_changed();
    }
}

/// The household's detector verdict, as a take's reply and the wake word screen show it.
async fn wake_detector_json(state: &AppState, phrase: &str) -> Value {
    let settings = state.settings_repo.get().await.unwrap_or_default();
    let takes = voice_prints(state)
        .wake_takes(phrase)
        .await
        .unwrap_or_default();
    let verdict = wake_verdict(&takes);
    let (kind, reason) = household_wake_detector(&settings, verdict.cnn_hears_household);
    json!({
        "kind": match kind {
            WakeDetector::HeyGoose => "hey_goose",
            WakeDetector::Whisper => "whisper",
        },
        "heard": verdict.heard,
        "of": verdict.of,
        "reason": reason,
        "missed": verdict.missed,
    })
}

struct Take {
    audio: Vec<u8>,
    prompt: Option<String>,
    profile_id: Option<String>,
    voice_match: bool,
    /// A take of the same run this one replaces: the sentence said again after the detector missed it.
    replaces: Option<String>,
}

async fn read_take(mut multipart: Multipart) -> Result<Take, Refusal> {
    let bad = |e: String| refuse(StatusCode::BAD_REQUEST, json!({"error": e}));
    let mut take = Take {
        audio: Vec::new(),
        prompt: None,
        profile_id: None,
        voice_match: true,
        replaces: None,
    };
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| bad(format!("multipart error: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "audio" => {
                take.audio = field
                    .bytes()
                    .await
                    .map_err(|e| bad(format!("read error: {e}")))?
                    .to_vec()
            }
            "prompt" => take.prompt = field.text().await.ok().filter(|p| !p.trim().is_empty()),
            "profile_id" => {
                take.profile_id = field.text().await.ok().filter(|p| !p.trim().is_empty())
            }
            "replaces" => take.replaces = field.text().await.ok().filter(|p| !p.trim().is_empty()),
            "voice_match" => {
                take.voice_match = !matches!(
                    field.text().await.unwrap_or_default().trim(),
                    "false" | "0" | "no"
                )
            }
            _ => {}
        }
    }
    if take.audio.is_empty() {
        return Err(bad("missing 'audio' field in multipart body".into()));
    }
    Ok(take)
}

/// `POST /api/v1/voice/takes`
pub(crate) async fn post_take(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Json<Value>, Refusal> {
    // Every sentence starts with the wake phrase: the room must not take the next one for a wake,
    // even from a phone that does not pause it.
    if let Some(room) = &state.room_voice {
        room.pause(std::time::Duration::from_secs(
            pond_core::models::domain::room_voice::PAUSE_AFTER_TAKE_SECONDS,
        ));
    }
    let take = read_take(multipart).await?;
    if take.voice_match && take.profile_id.is_none() {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "profile_required"}),
        ));
    }
    if let Some(id) = &take.profile_id {
        if state.profile_repo.get(id).await.map_err(failed)?.is_none() {
            return Err(refuse(
                StatusCode::NOT_FOUND,
                json!({"error": "profile_not_found"}),
            ));
        }
    }
    // A retake must replace a take of this member's current run, or the run would come apart.
    if let Some(replaces) = &take.replaces {
        let phrase = pond_voice::text::normalize_transcript(
            &state
                .settings_repo
                .get()
                .await
                .map_err(failed)?
                .voice_wake_word,
        );
        let takes = voice_prints(&state)
            .wake_takes(&phrase)
            .await
            .map_err(failed)?;
        if !replaceable(
            &takes,
            take.profile_id.as_deref(),
            replaces,
            chrono::Utc::now(),
        ) {
            tracing::warn!(
                kind = "voice_take_refused",
                reason = "retake_not_found",
                "voice: a retake named a take that is not in this member's current run"
            );
            return Err(refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error": "retake_not_found"}),
            ));
        }
    }
    let engine = engine(&state)?.clone();
    if take.voice_match
        && engine.model_status().state
            != pond_core::models::ports::voice_match::VoiceModelState::Ready
    {
        engine.fetch_model();
        return Err(refuse(
            StatusCode::CONFLICT,
            json!({"error": "model_not_ready"}),
        ));
    }

    let (samples, rate) = pond_voice::dsp::decode_wav(&take.audio)
        .map(|d| (d.samples, d.sample_rate))
        .map_err(|e| {
            refuse(
                StatusCode::BAD_REQUEST,
                json!({"error": format!("audio must be a WAV: {e}")}),
            )
        })?;
    let pcm = pond_voice::dsp::resample_to_16k(&samples, rate);

    let Some(transcribe) = state.transcribe_audio.clone() else {
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "speech_recognition_unavailable"}),
        ));
    };
    let audio = take.audio;
    let transcript = tokio::task::spawn_blocking(move || transcribe(audio))
        .await
        .map_err(failed)?
        .map_err(failed)?
        .trim()
        .to_string();
    if transcript.is_empty() {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "no_speech"}),
        ));
    }

    let mut settings = state.settings_repo.get().await.map_err(failed)?;
    let phrase = pond_voice::text::normalize_transcript(&settings.voice_wake_word);
    let Some(heard_as) = pond_voice::text::extract_wake_variant(
        &transcript,
        &settings.voice_wake_word,
        take.prompt.as_deref(),
    ) else {
        let words = phrase.split_whitespace().count().max(1);
        let opening: Vec<String> = pond_voice::text::normalize_transcript(&transcript)
            .split_whitespace()
            .take(words)
            .map(str::to_string)
            .collect();
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "no_wake_phrase", "heard_as": opening.join(" ")}),
        ));
    };
    if !settings.voice_wake_word_transcriptions.contains(&heard_as) {
        settings
            .voice_wake_word_transcriptions
            .push(heard_as.clone());
        state
            .settings_repo
            .update(&settings)
            .await
            .map_err(failed)?;
    }

    let heard = engine
        .hear_take(pcm, take.voice_match)
        .await
        .map_err(|e| match e {
            TakeError::ModelNotReady => {
                refuse(StatusCode::CONFLICT, json!({"error": "model_not_ready"}))
            }
            TakeError::Failed(why) => failed(why),
        })?;

    let store = voice_prints(&state);
    let now = chrono::Utc::now();
    if let Some(replaces) = &take.replaces {
        store.delete_wake_take(replaces).await.map_err(failed)?;
    }
    let take_id = uuid::Uuid::new_v4().to_string();
    store
        .add_wake_take(&WakeTake {
            id: take_id.clone(),
            profile_id: take.profile_id.clone(),
            phrase: phrase.clone(),
            cnn_heard: heard.cnn_heard,
            created_at: now,
        })
        .await
        .map_err(failed)?;

    let voice_match = match (heard.embedding, &take.profile_id) {
        (Some(embedding), Some(profile_id)) => {
            store
                .add_print(&VoicePrint {
                    id: uuid::Uuid::new_v4().to_string(),
                    profile_id: profile_id.clone(),
                    embedding,
                    model_name: engine.model_name().to_string(),
                    created_at: now,
                })
                .await
                .map_err(failed)?;
            let prints = store.prints(engine.model_name()).await.map_err(failed)?;
            let mine = members(&prints)
                .into_iter()
                .find(|m| &m.profile_id == profile_id);
            json!({
                "stored": true,
                "enrolled": mine.as_ref().is_some_and(|m| m.enrolled),
                "consistency": mine.and_then(|m| m.consistency),
            })
        }
        _ => Value::Null,
    };

    let takes = store.wake_takes(&phrase).await.map_err(failed)?;
    let take_number = latest_runs(&takes)
        .into_iter()
        .find(|run| run.profile_id == take.profile_id)
        .map(|run| run.takes)
        .unwrap_or(1);
    household_changed(&state);
    tracing::info!(
        kind = "voice_take",
        take = take_number,
        cnn_heard = heard.cnn_heard,
        voice_match = take.voice_match,
        retake = take.replaces.is_some(),
        "voice: an enrolment take was heard; its recording is not kept"
    );
    Ok(Json(json!({
        "take_id": take_id,
        "take": take_number,
        "of": TAKES_PER_RUN,
        "wake": {
            "heard": true,
            "heard_as": heard_as,
            "new_detector": heard.cnn_heard,
        },
        "voice_match": voice_match,
        "wake_detector": wake_detector_json(&state, &phrase).await,
    })))
}

/// `GET /api/v1/voice/prints`
pub(crate) async fn get_prints(State(state): State<Arc<AppState>>) -> Result<Json<Value>, Refusal> {
    let engine = engine(&state)?;
    let status = engine.model_status();
    let prints = voice_prints(&state)
        .prints(engine.model_name())
        .await
        .map_err(failed)?;
    let by_member = members(&prints);
    let profiles = state.profile_repo.list().await.map_err(failed)?;
    let rows: Vec<Value> = profiles
        .iter()
        .map(|profile| {
            let mine = by_member.iter().find(|m| m.profile_id == profile.id);
            json!({
                "profile_id": profile.id,
                "name": profile.display_name,
                "takes": mine.map_or(0, |m| m.takes),
                "needed": TAKES_PER_RUN,
                "enrolled": mine.is_some_and(|m| m.enrolled),
                "consistency": mine.and_then(|m| m.consistency),
                "updated_at": mine.and_then(|m| m.updated_at).map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    Ok(Json(json!({
        "model": {
            "state": status.state,
            "progress": status.progress,
            "credit": VOICE_MATCH_CREDIT,
        },
        "members": rows,
    })))
}

/// `DELETE /api/v1/voice/prints/{profile_id}`: their voice only; their face and wake takes stay.
pub(crate) async fn delete_prints(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, Refusal> {
    let deleted = voice_prints(&state)
        .delete_prints(&profile_id)
        .await
        .map_err(failed)?;
    tracing::info!(deleted, "voice: a member's voice prints were forgotten");
    household_changed(&state);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/voice/prints/model`: fetch the speaker model again.
pub(crate) async fn fetch_model(State(state): State<Arc<AppState>>) -> Response {
    match engine(&state) {
        Ok(engine) => {
            engine.fetch_model();
            (
                StatusCode::ACCEPTED,
                Json(json!({"state": engine.model_status().state})),
            )
                .into_response()
        }
        Err(refusal) => refusal.into_response(),
    }
}

/// `GET /api/v1/voice/prints/model/file`: the speaker model itself, for a phone that checks its
/// own wake phrase against the voices taught on it.
///
/// The bytes the Pond downloaded and verified, so a phone never fetches from the internet. The
/// phone pins the same checksum (`voice_match_model::SHA256`, also sent as `X-Model-Sha256`) and
/// refuses anything else. 409 `model_not_ready` while the Pond is still fetching it.
pub(crate) async fn model_file(State(state): State<Arc<AppState>>) -> Response {
    let engine = match engine(&state) {
        Ok(engine) => engine,
        Err(refusal) => return refusal.into_response(),
    };
    let status = engine.model_status();
    if status.state != VoiceModelState::Ready {
        return refuse(
            StatusCode::CONFLICT,
            json!({"error": "model_not_ready", "state": status.state}),
        )
        .into_response();
    }
    let Some(data_dir) = state.data_dir.as_ref() else {
        return failed("the Pond has no data directory").into_response();
    };
    let path = data_dir.join(voice_match_model::LOCAL_PATH);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            tracing::info!(
                kind = "voice_match_model",
                bytes = bytes.len(),
                "voice match: a phone fetched the speaker model"
            );
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
                    (
                        axum::http::HeaderName::from_static("x-model-sha256"),
                        voice_match_model::SHA256,
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(kind = "voice_match_model", error = %e, "voice match: the ready model could not be read");
            failed(e).into_response()
        }
    }
}

/// Forgets a member's voice prints and wake takes, for `DELETE /users/{id}/biometrics`.
pub(crate) async fn forget_voice(state: &AppState, profile_id: &str) -> anyhow::Result<u64> {
    let store = voice_prints(state);
    let deleted = store.delete_prints(profile_id).await?;
    store.delete_wake_takes(profile_id).await?;
    household_changed(state);
    Ok(deleted)
}

/// A change of wake phrase forgets what was learned of the old one: its pronunciations go with
/// the settings save, and the CNN's enrolment results here.
pub(crate) async fn wake_phrase_changed(state: &AppState) {
    match voice_prints(state).clear_wake_takes().await {
        Ok(cleared) => tracing::info!(
            cleared,
            "voice: the wake phrase changed; its calibration was cleared"
        ),
        Err(e) => tracing::warn!(error = %e, "voice: could not clear the old phrase's calibration"),
    }
    household_changed(state);
}
