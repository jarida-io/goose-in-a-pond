//! `POST /voice/wake/check`: a phone's segment is matched with the room's own triggers, the phrase
//! and its learned pronunciations as they are now, and only "was it the phrase" comes back.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::settings::SettingsRepository as _;
use serde_json::Value;
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    heard: Arc<Mutex<VecDeque<String>>>,
    settings: Arc<pond_infra::sqlite_settings::SqliteSettingsRepository>,
    _dir: tempfile::TempDir,
}

async fn a_pond(recognition: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let settings = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    state.settings_repo = settings.clone();
    let device_id = state
        .device_registry
        .register(RegisterDeviceRequest {
            id: None,
            name: "Phone".into(),
            device_type: "gotg".into(),
            hostname: None,
            capabilities: vec![],
            room_id: None,
        })
        .await
        .unwrap()
        .id;
    state.handshake = Arc::new(DeviceHandshake(device_id));
    let heard = Arc::new(Mutex::new(VecDeque::<String>::new()));
    if recognition {
        let heard = heard.clone();
        state.transcribe_audio = Some(Arc::new(move |_wav: Vec<u8>| {
            Ok(heard.lock().unwrap().pop_front().unwrap_or_default())
        }));
    }
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        heard,
        settings,
        _dir: built.dir,
    }
}

async fn set_phrase(pond: &Pond, phrase: &str, learned: &[&str]) {
    let mut s = pond.settings.get().await.unwrap();
    s.voice_wake_word = phrase.into();
    s.voice_wake_word_transcriptions = learned.iter().map(|v| v.to_string()).collect();
    pond.settings.update(&s).await.unwrap();
}

fn wav() -> Vec<u8> {
    pond_voice::dsp::encode_wav_pcm16(&pond_voice::dsp::f32_to_pcm16(&vec![0.0; 16_000]), 16_000)
}

async fn check(
    pond: &Pond,
    said: &str,
    audio: Vec<u8>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    *pond.heard.lock().unwrap() = VecDeque::from([said.to_string()]);
    let boundary = "wake-check-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"segment.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/voice/wake/check")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        );
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = pond
        .router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_custom_phrase_is_heard_as_said_and_as_learned_and_nothing_else_is() {
    let pond = a_pond(true).await;
    set_phrase(
        &pond,
        "Hey Jarvis",
        &["Hey Jarv, set a timer for ten minutes"],
    )
    .await;

    let (status, body) = check(
        &pond,
        "Hey Jarvis, what time is it?",
        wav(),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["heard"], true);
    assert_eq!(body["phrase"], "Hey Jarvis");

    let (status, body) = check(&pond, "Um, hey Jarv.", wav(), Some("test-token")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["heard"], true, "a learned pronunciation counts");

    let (status, body) = check(
        &pond,
        "I told Jarvis about it yesterday.",
        wav(),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["heard"], false, "the phrase must open the segment");

    let (status, body) = check(&pond, "", wav(), Some("test-token")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["heard"], false,
        "a segment with no words is not a wake"
    );
}

/// As [`check`], from a paired device, with `phrase_only=true`: a phone confirming its own detector.
async fn check_phrase_only(pond: &Pond, said: &str) -> (StatusCode, Value) {
    *pond.heard.lock().unwrap() = VecDeque::from([said.to_string()]);
    let boundary = "wake-check-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"segment.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&wav());
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"phrase_only\"\r\n\r\ntrue\r\n--{boundary}--\r\n").as_bytes(),
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/voice/wake/check")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("Authorization", "Bearer test-token");
    let response = pond
        .router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn phrase_only_wants_the_whole_phrase_where_the_room_takes_less() {
    let pond = a_pond(true).await;
    set_phrase(&pond, "hey goose", &["Hey gus, good morning."]).await;

    // The room's rule, which phones without the field keep: "OK goose" counts.
    let (status, body) = check(
        &pond,
        "OK goose, what time is it",
        wav(),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["heard"], true);

    for (said, heard) in [
        ("Hey goose, what time is it", true),
        ("hey gus, the lights", true),
        ("OK goose, what time is it", false),
        ("Goose.", false),
        ("hey hoose", false),
    ] {
        let (status, body) = check_phrase_only(&pond, said).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["heard"], heard, "{said}");
    }
}

#[tokio::test]
async fn a_phrase_changed_on_the_pond_counts_from_the_next_segment() {
    let pond = a_pond(true).await;
    set_phrase(&pond, "Hey Jarvis", &[]).await;
    let (_, body) = check(&pond, "Hey goose, lights on.", wav(), Some("test-token")).await;
    assert_eq!(body["heard"], false);

    set_phrase(&pond, "Hey Goose", &[]).await;
    let (status, body) = check(&pond, "Hey goose, lights on.", wav(), Some("test-token")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["heard"], true);
    assert_eq!(
        body["phrase"], "Hey Goose",
        "the phone learns it is back on its own detector"
    );
}

#[tokio::test]
async fn only_the_verdict_comes_back_never_what_was_said() {
    let pond = a_pond(true).await;
    set_phrase(&pond, "Hey Jarvis", &[]).await;
    let (status, body) = check(
        &pond,
        "Hey Jarvis, my pin is 4321",
        wav(),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let object = body.as_object().unwrap();
    assert_eq!(object.keys().collect::<Vec<_>>(), vec!["heard", "phrase"]);
    assert!(!body.to_string().contains("4321"));
}

#[tokio::test]
async fn it_needs_a_paired_device_a_wav_and_speech_recognition() {
    let pond = a_pond(true).await;
    let (status, _) = check(&pond, "Hey goose", wav(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = check(
        &pond,
        "Hey goose",
        b"not a wav".to_vec(),
        Some("test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "audio_must_be_wav");

    let deaf = a_pond(false).await;
    let (status, body) = check(&deaf, "Hey goose", wav(), Some("test-token")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "speech_recognition_unavailable");
}

#[tokio::test]
async fn a_segment_longer_than_the_ear_cuts_is_refused() {
    let pond = a_pond(true).await;
    let long = pond_voice::dsp::encode_wav_pcm16(
        &pond_voice::dsp::f32_to_pcm16(&vec![0.0; 16_000 * 12]),
        16_000,
    );
    let (status, _) = check(&pond, "Hey goose", long, Some("test-token")).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}
