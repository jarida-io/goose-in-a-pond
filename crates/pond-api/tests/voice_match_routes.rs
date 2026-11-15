//! Voice Match: an enrolment take teaches the wake word and the voice at once, what
//! `GET /voice/prints` shows, forgetting a voice or a face alone, and what a voice match may not do.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::models::ports::voice_match::{
    HeardTake, TakeError, VoiceMatchEngine, VoiceModelState, VoiceModelStatus,
};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
#[allow(unused_imports)]
use pond_core::user_data::ports::profile::ProfileRepository as _;
use pond_core::user_data::ports::voice_print::VoicePrintRepository as _;
use pond_infra::sqlite_voice_recognition::SqliteVoiceRecognition;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

/// Hears each take as scripted: whether the CNN fired, and an embedding near `voice`.
struct ScriptedEngine {
    ready: Mutex<bool>,
    cnn: Mutex<VecDeque<bool>>,
    voice: Vec<f32>,
    fetches: Mutex<usize>,
}

#[async_trait]
impl VoiceMatchEngine for ScriptedEngine {
    fn model_status(&self) -> VoiceModelStatus {
        VoiceModelStatus {
            state: if *self.ready.lock().unwrap() {
                VoiceModelState::Ready
            } else {
                VoiceModelState::Missing
            },
            progress: None,
        }
    }

    fn model_name(&self) -> &str {
        "test-model"
    }

    fn fetch_model(&self) {
        *self.fetches.lock().unwrap() += 1;
    }

    async fn hear_take(&self, _pcm: Vec<f32>, embed: bool) -> Result<HeardTake, TakeError> {
        if embed && !*self.ready.lock().unwrap() {
            return Err(TakeError::ModelNotReady);
        }
        Ok(HeardTake {
            cnn_heard: self.cnn.lock().unwrap().pop_front().unwrap_or(true),
            embedding: embed.then(|| self.voice.clone()),
        })
    }
}

struct Pond {
    router: axum::Router,
    amina: String,
    engine: Arc<ScriptedEngine>,
    heard: Arc<Mutex<VecDeque<String>>>,
    prints: SqliteVoiceRecognition,
    settings: Arc<pond_infra::sqlite_settings::SqliteSettingsRepository>,
    room: Arc<pond_core::models::mocks::mock_room_voice::MockRoomVoice>,
    _dir: tempfile::TempDir,
}

async fn pond(ready: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let settings = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    state.settings_repo = settings.clone();
    let amina = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Amina".into(),
            avatar_emoji: "duck".into(),
        })
        .await
        .unwrap()
        .id;
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
    let engine = Arc::new(ScriptedEngine {
        ready: Mutex::new(ready),
        cnn: Mutex::new(VecDeque::new()),
        voice: vec![1.0, 0.1, 0.0],
        fetches: Mutex::new(0),
    });
    state.voice_match = Some(engine.clone());
    let heard = Arc::new(Mutex::new(VecDeque::<String>::new()));
    state.transcribe_audio = Some({
        let heard = heard.clone();
        Arc::new(move |_wav: Vec<u8>| Ok(heard.lock().unwrap().pop_front().unwrap_or_default()))
    });
    let prints = SqliteVoiceRecognition::new(state.db.system.clone());
    let room = Arc::new(pond_core::models::mocks::mock_room_voice::MockRoomVoice::default());
    state.room_voice = Some(room.clone());
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        amina,
        engine,
        heard,
        prints,
        settings,
        room,
        _dir: built.dir,
    }
}

fn wav() -> Vec<u8> {
    pond_voice::dsp::encode_wav_pcm16(&pond_voice::dsp::f32_to_pcm16(&vec![0.0; 16_000]), 16_000)
}

async fn take(
    pond: &Pond,
    said: &str,
    profile: Option<&str>,
    voice_match: bool,
) -> (StatusCode, Value) {
    let mut fields = vec![("voice_match", if voice_match { "true" } else { "false" })];
    if let Some(profile) = profile {
        fields.push(("profile_id", profile));
    }
    post_take(pond, said, &fields).await
}

/// One sentence said again for `profile`, replacing the take `replaces`.
async fn retake(pond: &Pond, said: &str, profile: &str, replaces: &str) -> (StatusCode, Value) {
    post_take(
        pond,
        said,
        &[
            ("voice_match", "true"),
            ("profile_id", profile),
            ("replaces", replaces),
        ],
    )
    .await
}

async fn post_take(pond: &Pond, said: &str, fields: &[(&str, &str)]) -> (StatusCode, Value) {
    // A take refused before transcription leaves nothing queued for the next.
    *pond.heard.lock().unwrap() = VecDeque::from([said.to_string()]);
    let boundary = "voice-take-boundary";
    let mut body = Vec::new();
    let mut text = |name: &str, value: &str| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    text("prompt", "Hey goose, set a timer for ten minutes");
    for (name, value) in fields {
        text(name, value);
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"take.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&wav());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/voice/takes")
        .header("Authorization", "Bearer test-token")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    read(pond.router.clone().oneshot(request).await.unwrap()).await
}

async fn call(pond: &Pond, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token");
    let body = match body {
        Some(v) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    read(
        pond.router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap(),
    )
    .await
}

async fn read(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, body)
}

const SAID: &str = "Hey goose, set a timer for ten minutes.";

#[tokio::test]
async fn five_takes_teach_the_wake_word_and_the_voice_and_keep_no_audio() {
    let pond = pond(true).await;
    pond.engine
        .cnn
        .lock()
        .unwrap()
        .extend([true, true, false, true, true]);
    let mut last = Value::Null;
    for n in 1..=5 {
        let (status, body) = take(&pond, SAID, Some(&pond.amina), true).await;
        assert_eq!(status, StatusCode::OK, "take {n}: {body}");
        assert_eq!(body["take"], n, "{body}");
        assert_eq!(body["of"], 5);
        assert_eq!(body["wake"]["heard"], true);
        assert_eq!(body["wake"]["heard_as"], "hey goose");
        assert_eq!(body["voice_match"]["stored"], true);
        last = body;
    }
    assert_eq!(last["voice_match"]["enrolled"], true, "{last}");
    assert!(
        last["voice_match"]["consistency"].as_f64().unwrap() > 0.999,
        "{last}"
    );
    assert_eq!(last["wake"]["new_detector"], true);
    assert_eq!(last["wake_detector"]["heard"], 4);
    assert_eq!(last["wake_detector"]["of"], 5);
    assert_eq!(
        last["wake_detector"]["kind"], "hey_goose",
        "4 of 5 is enough: {last}"
    );
    assert_eq!(last["wake_detector"]["reason"], "calibrated");
    assert_eq!(pond.prints.prints("test-model").await.unwrap().len(), 5);

    let (status, body) = call(&pond, Method::GET, "/api/v1/voice/prints", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"]["state"], "ready");
    assert!(body["model"]["credit"]
        .as_str()
        .unwrap()
        .contains("CC BY 4.0"));
    let amina = &body["members"][0];
    assert_eq!(amina["name"], "Amina");
    assert_eq!(amina["takes"], 5);
    assert_eq!(amina["needed"], 5);
    assert_eq!(amina["enrolled"], true);
    assert!(amina["updated_at"].as_str().is_some());
}

#[tokio::test]
async fn a_missed_sentence_is_said_again_without_the_other_four() {
    let pond = pond(true).await;
    pond.engine
        .cnn
        .lock()
        .unwrap()
        .extend([true, false, true, true, false, true]);
    let mut ids = Vec::new();
    let mut last = Value::Null;
    for n in 1..=5 {
        let (status, body) = take(&pond, SAID, Some(&pond.amina), true).await;
        assert_eq!(status, StatusCode::OK, "take {n}: {body}");
        ids.push(body["take_id"].as_str().unwrap().to_string());
        last = body;
    }
    assert_eq!(last["wake_detector"]["heard"], 3, "{last}");
    assert_eq!(last["wake_detector"]["kind"], "whisper");

    let (status, body) = retake(&pond, SAID, &pond.amina, &ids[1]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["take"], 5, "the run is still one run of five: {body}");
    assert_eq!(body["wake"]["new_detector"], true);
    assert_eq!(body["wake_detector"]["heard"], 4, "{body}");
    assert_eq!(body["wake_detector"]["kind"], "hey_goose", "{body}");
    assert_ne!(body["take_id"], ids[1].as_str());

    // The replaced take is gone, so it cannot be replaced twice, and nobody else's can be.
    let (status, body) = retake(&pond, SAID, &pond.amina, &ids[1]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "retake_not_found");
    let (status, body) = retake(&pond, SAID, "someone-else", &ids[4]).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown member is refused first: {body}"
    );
}

#[tokio::test]
async fn a_refused_take_says_why_by_name() {
    let pond = pond(true).await;
    let (status, body) = take(&pond, SAID, None, true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "profile_required");

    let (status, body) = take(&pond, "", Some(&pond.amina), true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "no_speech");

    let (status, body) = take(&pond, "Lunch is ready.", Some(&pond.amina), true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "no_wake_phrase");
    assert_eq!(body["heard_as"], "lunch is");

    let unready = self::pond(false).await;
    let (status, body) = take(&unready, SAID, Some(&unready.amina), true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "model_not_ready");
    assert_eq!(
        *unready.engine.fetches.lock().unwrap(),
        1,
        "the model is fetched on first need"
    );

    // The wake word alone needs neither the speaker model nor a member.
    let (status, body) = take(&unready, SAID, None, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["voice_match"], Value::Null);
    assert!(pond.prints.prints("test-model").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_phone_fetches_the_ready_model_with_its_pinned_checksum() {
    let pond = pond(false).await;
    let (status, body) = call(&pond, Method::GET, "/api/v1/voice/prints/model/file", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "model_not_ready");

    let path = pond
        ._dir
        .path()
        .join(pond_core::models::domain::voice_match_model::LOCAL_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"speaker-model-bytes").unwrap();
    *pond.engine.ready.lock().unwrap() = true;
    let response = pond
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/voice/prints/model/file")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-model-sha256"],
        pond_core::models::domain::voice_match_model::SHA256
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"speaker-model-bytes");
}

#[tokio::test]
async fn the_model_is_fetched_again_on_request() {
    let pond = pond(false).await;
    let (status, body) = call(
        &pond,
        Method::POST,
        "/api/v1/voice/prints/model",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(*pond.engine.fetches.lock().unwrap(), 1);
    let (_, body) = call(&pond, Method::GET, "/api/v1/voice/prints", None).await;
    assert_eq!(body["model"]["state"], "missing");
    assert_eq!(body["members"][0]["takes"], 0);
    assert_eq!(body["members"][0]["consistency"], Value::Null);
}

#[tokio::test]
async fn forgetting_a_voice_leaves_the_face_and_forgetting_everything_takes_both() {
    let pond = pond(true).await;
    for _ in 0..2 {
        take(&pond, SAID, Some(&pond.amina), true).await;
    }
    let uri = format!("/api/v1/voice/prints/{}", pond.amina);
    let (status, _) = call(&pond, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(pond.prints.prints("test-model").await.unwrap().is_empty());
    assert_eq!(
        pond.prints.wake_takes("hey goose").await.unwrap().len(),
        2,
        "the wake calibration stays"
    );

    let (status, body) = call(
        &pond,
        Method::DELETE,
        &format!("/api/v1/faces/profile/{}", pond.amina),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["faces_deleted"], 0);

    take(&pond, SAID, Some(&pond.amina), true).await;
    let (status, body) = call(
        &pond,
        Method::DELETE,
        &format!("/api/v1/users/{}/biometrics", pond.amina),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["voice_prints_deleted"], 1);
    assert_eq!(body["face_embeddings_deleted"], 0);
    assert!(pond
        .prints
        .wake_takes("hey goose")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_new_wake_phrase_forgets_what_was_learned_of_the_old_one() {
    let pond = pond(true).await;
    take(&pond, SAID, Some(&pond.amina), true).await;
    use pond_core::user_data::ports::settings::SettingsRepository as _;
    assert!(!pond
        .settings
        .get()
        .await
        .unwrap()
        .voice_wake_word_transcriptions
        .is_empty());

    // Another member's run, or the same phrase saved again, resets nothing.
    let (status, _) = call(
        &pond,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"voice_wake_word": "Hey Goose"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pond.prints.wake_takes("hey goose").await.unwrap().len(), 1);

    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/settings",
        Some(json!({"voice_wake_word": "goose"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(pond
        .settings
        .get()
        .await
        .unwrap()
        .voice_wake_word_transcriptions
        .is_empty());
    assert!(pond
        .prints
        .wake_takes("hey goose")
        .await
        .unwrap()
        .is_empty());
}

/// A phone that does not pause the room still never wakes it with the next sentence.
#[tokio::test]
async fn a_take_deafens_the_room_for_twenty_seconds() {
    use pond_core::models::ports::room_voice::RoomVoiceControl as _;
    let pond = pond(true).await;
    let (status, _) = take(&pond, SAID, Some(&pond.amina), true).await;
    assert_eq!(status, StatusCode::OK);
    let status = pond.room.status();
    assert_eq!(
        status.state,
        pond_core::models::domain::room_voice::RoomState::Paused
    );
    let until = chrono::DateTime::parse_from_rfc3339(status.paused_until.as_deref().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(until - chrono::Utc::now() <= chrono::Duration::seconds(20));
}

/// A take the dashboard's calibration heard wrongly is taken back on its own, not with the rest.
#[tokio::test]
async fn one_wake_variant_is_forgotten_without_the_others() {
    use pond_core::user_data::ports::settings::SettingsRepository as _;
    let pond = pond(true).await;
    let mut settings = pond.settings.get().await.unwrap();
    settings.voice_wake_word_transcriptions = vec!["goose".into(), "gus".into(), "juice".into()];
    pond.settings.update(&settings).await.unwrap();

    let (status, body) = call(
        &pond,
        Method::DELETE,
        "/api/v1/voice/calibrate?variant=juice",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["removed"], "juice");
    assert_eq!(body["all_variants"], json!(["goose", "gus"]));
    assert_eq!(
        pond.settings
            .get()
            .await
            .unwrap()
            .voice_wake_word_transcriptions,
        vec!["goose".to_string(), "gus".to_string()]
    );

    let (status, body) = call(&pond, Method::DELETE, "/api/v1/voice/calibrate", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cleared"], true);
    assert!(pond
        .settings
        .get()
        .await
        .unwrap()
        .voice_wake_word_transcriptions
        .is_empty());
}
