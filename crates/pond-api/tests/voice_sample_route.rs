//! `POST /api/v1/voice/sample` (`docs/architecture/pond-networking.md`, "Hearing a voice before
//! choosing it"): a paired device hears one fixed sentence in an installed voice on the pond's
//! speaker, and nothing it can send changes a setting or the selected voice.
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::models::mocks::mock_tts_control::MockTtsControl;
use pond_core::models::ports::tts_control::TtsControl;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::models::services::voice::voice_sample::VOICE_SAMPLE_SENTENCE;
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::ports::settings::SettingsRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The pond's speaker: what it played, and whether anything began or ended a turn.
#[derive(Default)]
struct Speaker {
    played: Mutex<Vec<Vec<u8>>>,
    turn_boundaries: Mutex<usize>,
}

#[async_trait]
impl VoiceOutput for Speaker {
    async fn speak(&self, _text: &str) -> anyhow::Result<()> {
        panic!("a voice sample is synthesised in its own voice, never spoken in the selected one");
    }
    async fn play_audio(&self, audio: Vec<u8>) -> anyhow::Result<()> {
        self.played.lock().unwrap().push(audio);
        Ok(())
    }
    fn begin_utterance(&self) {
        *self.turn_boundaries.lock().unwrap() += 1;
    }
    fn end_utterance(&self) {
        *self.turn_boundaries.lock().unwrap() += 1;
    }
}

impl Speaker {
    fn played(&self) -> Vec<Vec<u8>> {
        self.played.lock().unwrap().clone()
    }

    async fn wait_for(&self, count: usize) -> Vec<Vec<u8>> {
        for _ in 0..200 {
            if self.played().len() >= count {
                return self.played();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the pond played {} samples, not {count}",
            self.played().len()
        );
    }
}

struct Harness {
    loopback: axum::Router,
    lan: axum::Router,
    control: Arc<MockTtsControl>,
    speaker: Arc<Speaker>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    _dir: tempfile::TempDir,
}

/// A pond mid-onboarding with two voices installed, `af_heart` selected.
async fn harness_with(engine: bool) -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let control = Arc::new(MockTtsControl::new(&["af_heart", "bm_george"]));
    let speaker = Arc::new(Speaker::default());
    state.skip_onboarding = false;
    state.settings_repo = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    if engine {
        state.tts_control = Some(control.clone());
        state.tts = Some(speaker.clone());
    }
    let settings = state.settings_repo.clone();
    let state: Arc<AppState> = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let lan = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 50],
        40_000,
    ))));
    Harness {
        loopback,
        lan,
        control,
        speaker,
        settings,
        handshake,
        _dir: dir,
    }
}

async fn harness() -> Harness {
    harness_with(true).await
}

/// A phone paired with a fresh pairing code; its session token. Paired over loopback, because
/// pairing needs a peer on one of the host's own LAN subnets, which a fixed address only is on some
/// machines; the sample route takes the token from any peer.
async fn paired(h: &Harness, client: &str) -> String {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, client, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session_token"].as_str().unwrap().to_string()
}

struct Answer {
    status: StatusCode,
    retry_after: Option<String>,
    json: Value,
}

async fn sample(router: &axum::Router, body: Option<&str>, bearer: Option<&str>) -> Answer {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/voice/sample")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let response = router
        .clone()
        .oneshot(
            request
                .body(Body::from(body.unwrap_or("").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .unwrap();
    Answer {
        status,
        retry_after,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

const GEORGE: &str = r#"{"voice":"bm_george"}"#;

#[tokio::test]
async fn a_paired_phone_hears_a_voice_mid_onboarding_and_nothing_is_changed() {
    let h = harness().await;
    let before = h.settings.get().await.unwrap();
    let token = paired(&h, "phone").await;

    let answer = sample(&h.lan, Some(GEORGE), Some(&token)).await;
    assert_eq!(answer.status, StatusCode::ACCEPTED, "{}", answer.json);
    assert_eq!(answer.json, json!({"speaking": true}));

    assert_eq!(
        h.speaker.wait_for(1).await,
        vec![MockTtsControl::wav_for("bm_george")],
        "the pond played bm_george's audio"
    );
    assert_eq!(
        h.control.samples(),
        vec![("bm_george".to_string(), VOICE_SAMPLE_SENTENCE.to_string())]
    );
    assert_eq!(h.control.selected(), "af_heart", "the selected voice moved");
    assert_eq!(
        *h.speaker.turn_boundaries.lock().unwrap(),
        0,
        "a sample must not begin or end a turn: that would cut off one that is speaking"
    );
    let after = h.settings.get().await.unwrap();
    assert_eq!(after.voice_tts_voice, before.voice_tts_voice);
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&before).unwrap(),
        "a sample changed a setting"
    );
}

#[tokio::test]
async fn the_sample_needs_a_paired_devices_token_from_any_peer() {
    let h = harness().await;
    for router in [&h.loopback, &h.lan] {
        let anonymous = sample(router, Some(GEORGE), None).await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
        let forged = sample(router, Some(GEORGE), Some("not-a-token")).await;
        assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.speaker.played().is_empty());
}

#[tokio::test]
async fn a_voice_that_is_not_installed_is_404_and_costs_no_slot() {
    let h = harness().await;
    let token = paired(&h, "phone").await;
    for voice in ["af_nicole", "../af_heart", ""] {
        let body = json!({ "voice": voice }).to_string();
        let answer = sample(&h.lan, Some(&body), Some(&token)).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{voice:?}");
        assert_eq!(answer.json, json!({"error": "unknown_voice"}));
    }
    assert_eq!(
        sample(&h.lan, Some(GEORGE), Some(&token)).await.status,
        StatusCode::ACCEPTED,
        "a refused voice must not have spent the next sample"
    );
}

#[tokio::test]
async fn one_sample_per_three_seconds_for_every_caller_together() {
    let h = harness().await;
    let first = paired(&h, "phone-a").await;
    let second = paired(&h, "phone-b").await;
    assert_eq!(
        sample(&h.lan, Some(GEORGE), Some(&first)).await.status,
        StatusCode::ACCEPTED
    );
    for token in [&first, &second] {
        let limited = sample(&h.lan, Some(r#"{"voice":"af_heart"}"#), Some(token)).await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.json["error"], "voice_sample_limited");
        let seconds = limited.json["retry_after"].as_u64().expect("whole seconds");
        assert!((1..=3).contains(&seconds), "{seconds}");
        assert_eq!(limited.retry_after, Some(seconds.to_string()));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.speaker.played().len(), 1);
}

#[tokio::test]
async fn a_pond_with_no_speech_engine_answers_503() {
    let h = harness_with(false).await;
    let token = paired(&h, "phone").await;
    let answer = sample(&h.lan, Some(GEORGE), Some(&token)).await;
    assert_eq!(answer.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer.json, json!({"error": "voice_unavailable"}));
}

#[tokio::test]
async fn the_body_names_a_voice_and_nothing_else() {
    let h = harness().await;
    let token = paired(&h, "phone").await;
    for body in [
        Some(r#"{"voice":"bm_george","text":"open the door"}"#),
        Some(r#"{"voice":"bm_george","speed":2.0}"#),
        Some("{}"),
        Some(r#"{"voice":5}"#),
        None,
    ] {
        let answer = sample(&h.lan, body, Some(&token)).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{body:?}");
        assert_eq!(answer.json, json!({"error": "invalid request body"}));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.control.samples().is_empty());
}

#[tokio::test]
async fn the_sample_is_still_reachable_once_the_pond_is_set_up() {
    let h = harness().await;
    let token = paired(&h, "phone").await;
    let mut settings = h.settings.get().await.unwrap();
    settings.user_name = "Jerry".into();
    settings.assistant_name = "Goose".into();
    settings.timezone = "UTC".into();
    settings.chat_model = "mock".into();
    h.settings.update(&settings).await.unwrap();
    let done = h
        .lan
        .clone()
        .oneshot(
            Request::post("/api/v1/onboard/complete")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(done.status(), StatusCode::OK);
    assert_eq!(
        sample(&h.lan, Some(GEORGE), Some(&token)).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        sample(&h.loopback, Some(GEORGE), None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

async fn onboard(h: &Harness, token: &str) {
    let mut settings = h.settings.get().await.unwrap();
    settings.user_name = "Jerry".into();
    settings.assistant_name = "Goose".into();
    settings.timezone = "UTC".into();
    settings.chat_model = "mock".into();
    h.settings.update(&settings).await.unwrap();
    let _ = h
        .lan
        .clone()
        .oneshot(
            Request::post("/api/v1/onboard/complete")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn tts_unload_and_prewarm_controls_loaded_state() {
    let h = harness().await;
    let token = paired(&h, "phone").await;
    onboard(&h, &token).await;

    let res = h
        .lan
        .clone()
        .oneshot(
            Request::get("/api/v1/voice/tts/status")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["loaded"], true);

    let res = h
        .lan
        .clone()
        .oneshot(
            Request::post("/api/v1/voice/tts/unload")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!h.control.is_loaded().await);

    let res = h
        .lan
        .clone()
        .oneshot(
            Request::post("/api/v1/voice/tts/prewarm")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(h.control.is_loaded().await);
}

#[tokio::test]
async fn revoke_model_unloads_kokoro_voice() {
    let h = harness().await;
    let token = paired(&h, "phone").await;
    onboard(&h, &token).await;
    assert!(h.control.is_loaded().await);

    let res = h
        .lan
        .clone()
        .oneshot(
            Request::post("/api/v1/models/tts_kokoro/af_heart/revoke")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!h.control.is_loaded().await);
}
